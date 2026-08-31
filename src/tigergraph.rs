use crate::data_prep::bench_capacity;
use crate::error::BenchmarkResult;
use crate::pokec_cypher_parser::{parse_edge_line, parse_node_line, EdgeRecord, NodeRecord};
use crate::tigergraph_client::{TigerGraphClient, TIGERGRAPH_GRAPH_NAME};
use futures::StreamExt;
use histogram::Histogram;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io;
use tokio::time::Instant;
use tracing::{error, info};

const SCHEMA_GSQL: &str = include_str!("tigergraph_gsql/schema.gsql");
const QUERIES_POINT_GSQL: &str = include_str!("tigergraph_gsql/queries_point.gsql");
const QUERIES_TRAVERSAL_GSQL: &str = include_str!("tigergraph_gsql/queries_traversal.gsql");
const QUERIES_PATHS_GSQL: &str = include_str!("tigergraph_gsql/queries_paths.gsql");
const QUERIES_AGGREGATE_GSQL: &str = include_str!("tigergraph_gsql/queries_aggregate.gsql");
const QUERIES_ALGO_GSQL: &str = include_str!("tigergraph_gsql/queries_algo.gsql");

/// Resolve TigerGraph connection parameters from environment variables, with sane local
/// defaults for a Community Edition Docker container: `(rest_base_url, gsql_base_url, username,
/// password)`.
pub fn default_connection_params() -> (String, String, String, String) {
    let host = std::env::var("TIGERGRAPH_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let rest_port = std::env::var("TIGERGRAPH_REST_PORT").unwrap_or_else(|_| "9000".to_string());
    let gsql_port = std::env::var("TIGERGRAPH_GSQL_PORT").unwrap_or_else(|_| "14240".to_string());
    let username =
        std::env::var("TIGERGRAPH_USERNAME").unwrap_or_else(|_| "tigergraph".to_string());
    let password =
        std::env::var("TIGERGRAPH_PASSWORD").unwrap_or_else(|_| "tigergraph".to_string());

    (
        format!("http://{}:{}", host, rest_port),
        format!("http://{}:{}", host, gsql_port),
        username,
        password,
    )
}

pub async fn clear_schema(client: &TigerGraphClient) -> BenchmarkResult<()> {
    // Order matters: the graph must be dropped before the edge/vertex types it references, and
    // the edge type before the vertex type it connects.
    let script = format!(
        "DROP GRAPH {graph}\nDROP EDGE Friend\nDROP VERTEX User\n",
        graph = TIGERGRAPH_GRAPH_NAME
    );
    client.execute_gsql_script(&script).await
}

pub async fn create_schema(client: &TigerGraphClient) -> BenchmarkResult<()> {
    client.execute_gsql_script(SCHEMA_GSQL).await?;
    for script in [
        QUERIES_POINT_GSQL,
        QUERIES_TRAVERSAL_GSQL,
        QUERIES_PATHS_GSQL,
        QUERIES_AGGREGATE_GSQL,
        QUERIES_ALGO_GSQL,
    ] {
        client.execute_gsql_script(script).await?;
    }
    // Compiles and installs every `CREATE QUERY` defined above as a callable REST++ endpoint.
    client.execute_gsql_script("INSTALL QUERY ALL\n").await?;
    Ok(())
}

fn nodes_upsert_body(nodes: &[NodeRecord]) -> Value {
    let mut vertex_map = Map::new();
    for n in nodes {
        vertex_map.insert(
            n.id.to_string(),
            json!({
                "completion_percentage": {"value": n.completion_percentage},
                "gender": {"value": n.gender},
                "age": {"value": n.age},
            }),
        );
    }
    json!({ "vertices": { "User": Value::Object(vertex_map) } })
}

fn edges_upsert_body(edges: &[EdgeRecord]) -> Value {
    let mut by_src: HashMap<i32, Map<String, Value>> = HashMap::new();
    for e in edges {
        let capacity = bench_capacity(e.src as u64, e.dst as u64);
        by_src.entry(e.src).or_default().insert(
            e.dst.to_string(),
            json!({ "bench_capacity": {"value": capacity} }),
        );
    }

    let mut src_map = Map::new();
    for (src, dst_map) in by_src {
        src_map.insert(
            src.to_string(),
            json!({ "Friend": { "User": Value::Object(dst_map) } }),
        );
    }
    json!({ "edges": { "User": Value::Object(src_map) } })
}

async fn flush_nodes(
    client: &TigerGraphClient,
    nodes: &mut Vec<NodeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    *batch_count += 1;

    let body = nodes_upsert_body(nodes);
    let start = Instant::now();
    client.upsert_graph_data(&body).await?;
    histogram.increment(start.elapsed().as_micros() as u64)?;
    nodes.clear();
    Ok(())
}

async fn flush_edges(
    client: &TigerGraphClient,
    edges: &mut Vec<EdgeRecord>,
    histogram: &mut Histogram,
    batch_count: &mut usize,
) -> BenchmarkResult<()> {
    if edges.is_empty() {
        return Ok(());
    }
    *batch_count += 1;

    let body = edges_upsert_body(edges);
    let start = Instant::now();
    client.upsert_graph_data(&body).await?;
    histogram.increment(start.elapsed().as_micros() as u64)?;
    edges.clear();
    Ok(())
}

/// Fast-path loader for the Pokec "Users" dataset, mirroring `postgres::load_from_cypher_import`'s
/// two-phase (nodes-then-edges) batching strategy but upserting via TigerGraph's REST++
/// `POST /graph/{graph}` endpoint instead of bulk-insert SQL.
pub async fn load_from_cypher_import<S>(
    client: &TigerGraphClient,
    mut stream: S,
    batch_size: usize,
    histogram: &mut Histogram,
) -> BenchmarkResult<usize>
where
    S: StreamExt<Item = Result<String, io::Error>> + Unpin,
{
    info!(
        "Processing Pokec Users import into TigerGraph in batches of {}",
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
        "Pokec Users import into TigerGraph completed: {} records batched into {} REST++ upsert calls",
        total_processed, batch_count
    );

    Ok(total_processed)
}
