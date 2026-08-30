use crate::data_prep::bench_capacity;
use crate::error::BenchmarkError::MongoError;
use crate::error::BenchmarkResult;
use crate::mongo_client::MongoClient;
use crate::pokec_cypher_parser::{parse_edge_line, parse_node_line, EdgeRecord, NodeRecord};
use futures::StreamExt;
use histogram::Histogram;
use mongodb::bson::{doc, Document};
use mongodb::options::InsertManyOptions;
use std::io;
use tokio::time::Instant;
use tracing::{error, info};

/// Resolve a Mongo connection URI + database name from environment variables, with sane local
/// defaults. `MONGO_URI` takes priority (allows full connection strings, e.g. `mongodb+srv://...`
/// or replica-set/auth options); otherwise a URI is built from host/port/user/password.
pub fn default_connection_params() -> (String, String) {
    let dbname = std::env::var("MONGO_DB").unwrap_or_else(|_| "benchmark".to_string());

    if let Ok(uri) = std::env::var("MONGO_URI") {
        return (uri, dbname);
    }

    let host = std::env::var("MONGO_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("MONGO_PORT").unwrap_or_else(|_| "27017".to_string());
    let user = std::env::var("MONGO_USER").unwrap_or_default();
    let password = std::env::var("MONGO_PASSWORD").unwrap_or_default();

    let uri = if user.is_empty() {
        format!("mongodb://{}:{}", host, port)
    } else {
        format!("mongodb://{}:{}@{}:{}", user, password, host, port)
    };

    (uri, dbname)
}

pub async fn clear_schema(client: &MongoClient) -> BenchmarkResult<()> {
    client.drop_collections().await
}

pub async fn create_schema(client: &MongoClient) -> BenchmarkResult<()> {
    client.create_schema().await
}

fn node_to_document(n: &NodeRecord) -> Document {
    doc! {
        "_id": n.id,
        "completion_percentage": n.completion_percentage,
        "gender": n.gender.clone(),
        "age": n.age,
    }
}

fn edge_to_document(e: &EdgeRecord) -> Document {
    doc! {
        "src": e.src,
        "dst": e.dst,
        "bench_capacity": bench_capacity(e.src as u64, e.dst as u64) as i32,
    }
}

async fn flush_nodes(
    client: &MongoClient,
    nodes: &mut Vec<NodeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    *batch_count += 1;

    let docs: Vec<Document> = nodes.iter().map(node_to_document).collect();
    let start = Instant::now();
    let options = InsertManyOptions::builder().ordered(false).build();
    if let Err(e) = client.users_collection().insert_many(docs, options).await {
        // Ignore duplicate-key errors (E11000) so re-running against a partially loaded
        // collection behaves like Postgres's `ON CONFLICT (id) DO NOTHING`.
        if !e.to_string().contains("E11000") {
            return Err(MongoError(e));
        }
    }
    histogram.increment(start.elapsed().as_micros() as u64)?;
    nodes.clear();
    Ok(())
}

async fn flush_edges(
    client: &MongoClient,
    edges: &mut Vec<EdgeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if edges.is_empty() {
        return Ok(());
    }
    *batch_count += 1;

    let docs: Vec<Document> = edges.iter().map(edge_to_document).collect();
    let start = Instant::now();
    let options = InsertManyOptions::builder().ordered(false).build();
    if let Err(e) = client.friend_edges_collection().insert_many(docs, options).await {
        if !e.to_string().contains("E11000") {
            return Err(MongoError(e));
        }
    }
    histogram.increment(start.elapsed().as_micros() as u64)?;
    edges.clear();
    Ok(())
}

/// Fast-path loader for the Pokec "Users" dataset, mirroring `postgres::load_from_cypher_import`'s
/// batching strategy but emitting `insertMany` calls instead of bulk-insert SQL.
pub async fn load_from_cypher_import<S>(
    client: &MongoClient,
    mut stream: S,
    batch_size: usize,
    histogram: &mut Histogram,
) -> BenchmarkResult<usize>
where
    S: StreamExt<Item = Result<String, io::Error>> + Unpin,
{
    info!(
        "Processing Pokec Users import into MongoDB in batches of {}",
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
        "Pokec Users import into MongoDB completed: {} records batched into {} insertMany calls",
        total_processed, batch_count
    );

    Ok(total_processed)
}
