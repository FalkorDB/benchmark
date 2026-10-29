use crate::data_prep::bench_capacity;
use crate::error::BenchmarkResult;
use crate::pokec_cypher_parser::{parse_edge_line, parse_node_line, EdgeRecord, NodeRecord};
use crate::spanner_client::SpannerClient;
use futures::StreamExt;
use histogram::Histogram;
use std::io;
use tokio::time::Instant;
use tracing::{error, info};

/// Default property-graph name used by generated GQL statements.
pub const DEFAULT_GRAPH_NAME: &str = "PokecGraph";

/// Resolve Spanner connection parameters from environment variables.
///
/// Required:
/// - `SPANNER_DATABASE` — full resource path
///   `projects/<p>/instances/<i>/databases/<d>`
///
/// Optional:
/// - `SPANNER_GRAPH_NAME` — property graph name (default `PokecGraph`)
///
/// Auth uses Application Default Credentials (`GOOGLE_APPLICATION_CREDENTIALS` or metadata).
/// For the emulator, set `SPANNER_EMULATOR_HOST` (handled by the Spanner client library).
pub fn default_connection_params() -> BenchmarkResult<(String, String)> {
    let database = std::env::var("SPANNER_DATABASE").map_err(|_| {
        crate::error::BenchmarkError::OtherError(
            "SPANNER_DATABASE is required (projects/<p>/instances/<i>/databases/<d>)".to_string(),
        )
    })?;
    let graph_name =
        std::env::var("SPANNER_GRAPH_NAME").unwrap_or_else(|_| DEFAULT_GRAPH_NAME.to_string());
    Ok((database, graph_name))
}

/// Drop the property graph and underlying input tables (best-effort order).
pub async fn clear_schema(client: &SpannerClient) -> BenchmarkResult<()> {
    let graph = client.graph_name();
    // DROP PROPERTY GRAPH first so the tables are no longer referenced, then tables.
    let _ = client
        .update_ddl(&[format!("DROP PROPERTY GRAPH IF EXISTS {}", graph)])
        .await;
    client
        .update_ddl(&[
            "DROP TABLE IF EXISTS FriendEdges".to_string(),
            "DROP TABLE IF EXISTS Users".to_string(),
        ])
        .await?;
    Ok(())
}

/// Create Users/FriendEdges tables + property graph + reverse-edge index.
///
/// Edge PK is `(src_id, dst_id)` to match Postgres/Mongo uniqueness for Pokec (one directed
/// friendship per pair). Spanner Graph maps the tables via `CREATE PROPERTY GRAPH`.
pub async fn create_schema(client: &SpannerClient) -> BenchmarkResult<()> {
    let graph = client.graph_name();
    let statements = vec![
        r#"CREATE TABLE IF NOT EXISTS Users (
            id INT64 NOT NULL,
            completion_percentage INT64,
            gender STRING(MAX),
            age INT64
        ) PRIMARY KEY (id)"#
        .to_string(),
        r#"CREATE TABLE IF NOT EXISTS FriendEdges (
            src_id INT64 NOT NULL,
            dst_id INT64 NOT NULL,
            bench_capacity INT64,
            FOREIGN KEY (dst_id) REFERENCES Users (id)
        ) PRIMARY KEY (src_id, dst_id),
          INTERLEAVE IN PARENT Users ON DELETE CASCADE"#
        .to_string(),
        // Reverse-edge index: optimizes inbound Friend traversals (Spanner Graph best practice).
        "CREATE INDEX IF NOT EXISTS FriendEdgesByDst ON FriendEdges(dst_id, src_id)".to_string(),
        format!(
            r#"CREATE OR REPLACE PROPERTY GRAPH {graph}
  NODE TABLES (
    Users
      KEY (id)
      LABEL User
      PROPERTIES (id, completion_percentage, gender, age)
  )
  EDGE TABLES (
    FriendEdges
      KEY (src_id, dst_id)
      SOURCE KEY (src_id) REFERENCES Users (id)
      DESTINATION KEY (dst_id) REFERENCES Users (id)
      LABEL Friend
      PROPERTIES (src_id, dst_id, bench_capacity)
  )"#
        ),
    ];
    client.update_ddl(&statements).await?;
    Ok(())
}

async fn flush_nodes(
    client: &SpannerClient,
    nodes: &mut Vec<NodeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    *batch_count += 1;
    let batch = std::mem::take(nodes);
    let start = Instant::now();
    client.insert_users_batch(&batch).await?;
    histogram.increment(start.elapsed().as_micros() as u64)?;
    Ok(())
}

async fn flush_edges(
    client: &SpannerClient,
    edges: &mut Vec<EdgeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if edges.is_empty() {
        return Ok(());
    }
    *batch_count += 1;
    let batch: Vec<(i64, i64, i64)> = edges
        .drain(..)
        .map(|e| {
            (
                e.src as i64,
                e.dst as i64,
                bench_capacity(e.src as u64, e.dst as u64),
            )
        })
        .collect();
    let start = Instant::now();
    client.insert_friend_edges_batch(&batch).await?;
    histogram.increment(start.elapsed().as_micros() as u64)?;
    Ok(())
}

/// Fast-path loader for the Pokec "Users" dataset, mirroring Postgres/Mongo batching but
/// emitting Spanner mutations (`insert_or_update`) instead of bulk SQL/`insertMany`.
pub async fn load_from_cypher_import<S>(
    client: &SpannerClient,
    mut stream: S,
    batch_size: usize,
    histogram: &mut Histogram,
) -> BenchmarkResult<usize>
where
    S: StreamExt<Item = Result<String, io::Error>> + Unpin,
{
    info!(
        "Processing Pokec Users import into Spanner Graph in batches of {}",
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
        "Pokec Users import into Spanner completed: {} records batched into {} mutation applies",
        total_processed, batch_count
    );

    Ok(total_processed)
}
