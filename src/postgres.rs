use crate::data_prep::bench_capacity;
use crate::error::BenchmarkResult;
use crate::pokec_cypher_parser::{parse_edge_line, parse_node_line, EdgeRecord, NodeRecord};
use crate::postgres_client::PostgresClient;
use futures::StreamExt;
use histogram::Histogram;
use std::io;
use tokio::time::Instant;
use tracing::{error, info};

/// Resolve Postgres connection parameters from environment variables, with sane local defaults.
pub fn default_connection_params() -> (String, u16, String, String, String) {
    let host = std::env::var("POSTGRES_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("POSTGRES_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(5432);
    let user = std::env::var("POSTGRES_USER").unwrap_or_else(|_| "postgres".to_string());
    let password = std::env::var("POSTGRES_PASSWORD").unwrap_or_default();
    let dbname = std::env::var("POSTGRES_DB").unwrap_or_else(|_| "postgres".to_string());
    (host, port, user, password, dbname)
}

pub async fn clear_schema(client: &PostgresClient) -> BenchmarkResult<()> {
    client.execute_ddl("DROP TABLE IF EXISTS friend_edges").await?;
    client.execute_ddl("DROP TABLE IF EXISTS users").await?;
    Ok(())
}

pub async fn create_schema(client: &PostgresClient) -> BenchmarkResult<()> {
    client
        .execute_ddl(
            "CREATE TABLE IF NOT EXISTS users ( \
                id INTEGER PRIMARY KEY, \
                completion_percentage INTEGER, \
                gender TEXT, \
                age INTEGER, \
                rpc_social_credit INTEGER, \
                created_at TIMESTAMPTZ, \
                last_seen TIMESTAMPTZ, \
                loop_counter INTEGER, \
                bench_temp_flag BOOLEAN \
            )",
        )
        .await?;

    client
        .execute_ddl(
            "CREATE TABLE IF NOT EXISTS friend_edges ( \
                src_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, \
                dst_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, \
                color INTEGER, \
                bench_capacity INTEGER, \
                since DATE, \
                touch DATE, \
                PRIMARY KEY (src_id, dst_id) \
            )",
        )
        .await?;

    client
        .execute_ddl("CREATE INDEX IF NOT EXISTS friend_edges_dst_id_idx ON friend_edges (dst_id)")
        .await?;
    client
        .execute_ddl("CREATE INDEX IF NOT EXISTS users_age_idx ON users (age)")
        .await?;

    Ok(())
}

fn escape_sql_literal(s: &str) -> String {
    s.replace('\'', "''")
}

async fn flush_nodes(
    client: &PostgresClient,
    nodes: &mut Vec<NodeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    *batch_count += 1;

    let mut sql = String::from(
        "INSERT INTO users (id, completion_percentage, gender, age) VALUES ",
    );
    for (i, n) in nodes.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push_str(&format!(
            "({}, {}, '{}', {})",
            n.id,
            n.completion_percentage,
            escape_sql_literal(&n.gender),
            n.age
        ));
    }
    sql.push_str(" ON CONFLICT (id) DO NOTHING");

    let start = Instant::now();
    client.execute_ddl(&sql).await?;
    histogram.increment(start.elapsed().as_micros() as u64)?;
    nodes.clear();
    Ok(())
}

async fn flush_edges(
    client: &PostgresClient,
    edges: &mut Vec<EdgeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if edges.is_empty() {
        return Ok(());
    }
    *batch_count += 1;

    let mut sql = String::from("INSERT INTO friend_edges (src_id, dst_id, bench_capacity) VALUES ");
    for (i, e) in edges.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        let capacity = bench_capacity(e.src as u64, e.dst as u64);
        sql.push_str(&format!("({}, {}, {})", e.src, e.dst, capacity));
    }
    sql.push_str(" ON CONFLICT (src_id, dst_id) DO NOTHING");

    let start = Instant::now();
    client.execute_ddl(&sql).await?;
    histogram.increment(start.elapsed().as_micros() as u64)?;
    edges.clear();
    Ok(())
}

/// Fast-path loader for the Pokec "Users" dataset, mirroring
/// `memgraph_client::execute_pokec_users_import_unwind`'s batching strategy but emitting raw
/// bulk-insert SQL instead of Cypher UNWIND statements.
pub async fn load_from_cypher_import<S>(
    client: &PostgresClient,
    mut stream: S,
    batch_size: usize,
    histogram: &mut Histogram,
) -> BenchmarkResult<usize>
where
    S: StreamExt<Item = Result<String, io::Error>> + Unpin,
{
    info!(
        "Processing Pokec Users import into Postgres in batches of {}",
        batch_size
    );

    #[derive(Copy, Clone, PartialEq, Eq)]
    enum Phase {
        Nodes,
        Edges,
    }

    let mut phase = Phase::Nodes;
    let mut nodes: Vec<NodeRecord> = Vec::with_capacity(batch_size);
    let mut edges: Vec<EdgeRecord> = Vec::with_capacity(batch_size);

    let mut total_processed: usize = 0;
    let mut batch_count: usize = 0;

    while let Some(item_result) = stream.next().await {
        let line = match item_result {
            Ok(v) => v,
            Err(e) => {
                error!("Error reading import line: {:?}", e);
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == ";" || trimmed.starts_with("//") {
            continue;
        }

        if phase == Phase::Nodes && trimmed.starts_with("MATCH") {
            flush_nodes(client, &mut nodes, histogram, &mut batch_count).await?;
            phase = Phase::Edges;
        }

        match phase {
            Phase::Nodes => {
                if let Some(node) = parse_node_line(trimmed) {
                    nodes.push(node);
                    total_processed += 1;
                }
                if nodes.len() >= batch_size {
                    flush_nodes(client, &mut nodes, histogram, &mut batch_count).await?;
                }
            }
            Phase::Edges => {
                if let Some(edge) = parse_edge_line(trimmed) {
                    edges.push(edge);
                    total_processed += 1;
                }
                if edges.len() >= batch_size {
                    flush_edges(client, &mut edges, histogram, &mut batch_count).await?;
                }
            }
        }
    }

    flush_nodes(client, &mut nodes, histogram, &mut batch_count).await?;
    flush_edges(client, &mut edges, histogram, &mut batch_count).await?;

    info!(
        "Pokec Users import into Postgres completed: {} records batched into {} bulk-insert statements",
        total_processed, batch_count
    );

    Ok(total_processed)
}
