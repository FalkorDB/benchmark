//! One measured invocation of an operation, and the loop that collects many of them.
//!
//! [`run_and_drain`] times a single query end-to-end (the wall-clock around
//! `execute().await` *and* draining every row) and reads FalkorDB's own reported internal
//! execution time from the response. A missing server-time statistic is an error rather than a
//! silent `NaN`, so it can never poison the summary.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::queries_repository::QueryType;
use crate::synthetic::writes::MutationStats;
use falkordb::{AsyncGraph, Edge, FalkorValue, Node};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::hint::black_box;
use std::time::{Duration, Instant};
use tokio::time::error::Elapsed;

/// A single paired latency measurement for one operation invocation.
#[derive(Debug, Clone, Copy)]
pub struct OpSample {
    /// FalkorDB's reported internal execution time, in milliseconds.
    pub server_ms: f64,
    /// End-to-end client wall-clock (send → response received → all rows drained), in milliseconds.
    pub total_ms: f64,
    /// Number of rows drained from the result.
    pub rows: usize,
    /// Whether the server reported a cached execution plan (`None` if the stat was absent).
    pub cached: Option<bool>,
    /// The mutation counters FalkorDB reported (all zero for reads), so a write worker can verify
    /// each sample actually effected its intended change (see [`crate::synthetic::writes`]).
    pub mutations: MutationStats,
}

/// Execute one query against `graph`, timing the full round-trip and reading the server time.
///
/// `cypher` is the already-rendered query string (the caller controls its exact text, e.g. to
/// force a plan-cache miss). `server_timeout_ms` is the FalkorDB-side per-query guard;
/// `client_deadline` bounds the **entire** operation — execute *and* row draining — so a stuck
/// socket or a slow stream can't hang the probe. Reads use `GRAPH.RO_QUERY`, writes `GRAPH.QUERY`.
pub async fn run_and_drain(
    graph: &mut AsyncGraph,
    kind: QueryType,
    cypher: &str,
    server_timeout_ms: i64,
    client_deadline: Duration,
) -> BenchmarkResult<OpSample> {
    let started = Instant::now();

    // The whole operation (execute + drain) runs under one deadline.
    let measured = async {
        let query_result = match kind {
            QueryType::Read => {
                graph
                    .ro_query(cypher)
                    .with_timeout(server_timeout_ms)
                    .execute()
                    .await
            }
            QueryType::Write => {
                graph
                    .query(cypher)
                    .with_timeout(server_timeout_ms)
                    .execute()
                    .await
            }
        }
        .map_err(|e| OtherError(format!("query '{}' failed: {:?}", cypher, e)))?;

        let cached = query_result.get_cached_execution();
        // Read the mutation counters (absent ⇒ 0, e.g. for reads) before draining the stream, so a
        // write worker can verify the sample actually did what the operation intends.
        let mutations = MutationStats {
            nodes_created: query_result.get_nodes_created().unwrap_or(0),
            nodes_deleted: query_result.get_nodes_deleted().unwrap_or(0),
            relationships_created: query_result.get_relationship_created().unwrap_or(0),
            properties_set: query_result.get_properties_set().unwrap_or(0),
        };
        // A missing server-time stat is a hard error — never fold it into the numbers as NaN/0.
        let server_ms = validate_server_ms(query_result.get_internal_execution_time(), cypher)?;

        // Drain every row so `total_ms` reflects full client-side consumption and any row-decode
        // error surfaces here rather than being silently skipped.
        let mut rows = 0usize;
        let mut data = query_result.data;
        while let Some(row) = data.next().await {
            let row =
                row.map_err(|e| OtherError(format!("row decode error for '{}': {:?}", cypher, e)))?;
            let _ = black_box(row);
            rows += 1;
        }
        Ok::<(f64, usize, Option<bool>, MutationStats), crate::error::BenchmarkError>((
            server_ms, rows, cached, mutations,
        ))
    };

    let (server_ms, rows, cached, mutations) = tokio::time::timeout(client_deadline, measured)
        .await
        .map_err(|e: Elapsed| {
            OtherError(format!(
                "client deadline ({} ms) exceeded for query '{}': {}",
                client_deadline.as_millis(),
                cypher,
                e
            ))
        })??;

    let total_ms = started.elapsed().as_secs_f64() * 1_000.0;
    Ok(OpSample {
        server_ms,
        total_ms,
        rows,
        cached,
        mutations,
    })
}

/// A canonical, order-independent fingerprint of a read query's result set: the row count plus a
/// SHA-256 over the **sorted** canonical rendering of every row. Used by the untimed correctness
/// passes (reference + concurrency-invariance check) — never on the timed measurement path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultShape {
    /// Number of rows returned.
    pub rows: usize,
    /// `sha256:…` over the sorted per-row canonical strings (order-independent).
    pub value_digest: String,
}

/// Execute a **read** query and capture its [`ResultShape`] (row count + order-independent value
/// digest), bounded by `client_deadline`. Rows are rendered to canonical strings, sorted, then
/// hashed, so a differing row *order* doesn't matter but differing *values* or *cardinality* do.
pub async fn capture_result(
    graph: &mut AsyncGraph,
    cypher: &str,
    server_timeout_ms: i64,
    client_deadline: Duration,
) -> BenchmarkResult<ResultShape> {
    let fut = async {
        let query_result = graph
            .ro_query(cypher)
            .with_timeout(server_timeout_ms)
            .execute()
            .await
            .map_err(|e| OtherError(format!("capture query '{}' failed: {:?}", cypher, e)))?;
        let mut rows: Vec<String> = Vec::new();
        let mut data = query_result.data;
        while let Some(row) = data.next().await {
            let row =
                row.map_err(|e| OtherError(format!("row decode error for '{}': {:?}", cypher, e)))?;
            rows.push(canonical_row(&row.into_values()));
        }
        Ok::<Vec<String>, crate::error::BenchmarkError>(rows)
    };
    let mut rows = tokio::time::timeout(client_deadline, fut)
        .await
        .map_err(|e: Elapsed| {
            OtherError(format!(
                "client deadline ({} ms) exceeded capturing '{}': {}",
                client_deadline.as_millis(),
                cypher,
                e
            ))
        })??;
    let count = rows.len();
    rows.sort_unstable();
    let mut h = Sha256::new();
    h.update((count as u64).to_le_bytes());
    for r in &rows {
        h.update((r.len() as u64).to_le_bytes());
        h.update(r.as_bytes());
    }
    Ok(ResultShape {
        rows: count,
        value_digest: format!("sha256:{:x}", h.finalize()),
    })
}

/// Canonicalize one row (its column values, in column order) to a stable string. Each column is
/// **length-prefixed** (`<byte-len>:<value>`) so no column value — even one containing a delimiter
/// byte — can shift the boundary and alias a different row.
fn canonical_row(values: &[FalkorValue]) -> String {
    let mut s = String::new();
    for v in values {
        let cv = canonical_value(v);
        s.push_str(&cv.len().to_string());
        s.push(':');
        s.push_str(&cv);
    }
    s
}

/// A stable, **recursive** canonical string for any [`FalkorValue`] — including the compound shapes
/// (nodes, edges, paths, maps, arrays, vectors, points, temporals) our read ops can return.
///
/// The invariant that makes the result digest deterministic across processes: **`Map` and property
/// keys are sorted** (a `HashMap`'s iteration order is randomized per process —
/// `vendor/falkordb-rs/src/value/graph_entities.rs`), while **array and path order is preserved**
/// (those carry meaningful, server-stable order). Every nested value is length-prefixed
/// (`<byte-len>:<value>`) and each variant carries a distinct type tag, so no value can alias a
/// structurally different one. Floats (and `f32` vector lanes / point coordinates) use their bit
/// pattern so equal values always render identically.
fn canonical_value(v: &FalkorValue) -> String {
    match v {
        FalkorValue::I64(i) => format!("i:{i}"),
        FalkorValue::F64(f) => format!("f:{:016x}", f.to_bits()),
        FalkorValue::Bool(b) => format!("b:{b}"),
        FalkorValue::String(s) => format!("s:{s}"),
        FalkorValue::None => "null".to_string(),
        FalkorValue::Unparseable(s) => format!("unparseable:{s}"),
        FalkorValue::Node(n) => canonical_node(n),
        FalkorValue::Edge(e) => canonical_edge(e),
        FalkorValue::Path(p) => {
            // A path's node and relationship sequences are ordered — preserve both.
            let mut nodes = String::new();
            for n in &p.nodes {
                push_field(&mut nodes, &canonical_node(n));
            }
            let mut rels = String::new();
            for r in &p.relationships {
                push_field(&mut rels, &canonical_edge(r));
            }
            let mut s = String::new();
            push_field(&mut s, &nodes);
            push_field(&mut s, &rels);
            format!("path[{s}]")
        }
        FalkorValue::Array(items) => {
            // Array order is meaningful (and server-stable) — preserve it.
            let mut s = String::new();
            for item in items {
                push_field(&mut s, &canonical_value(item));
            }
            format!("arr[{s}]")
        }
        FalkorValue::Map(m) => format!("map[{}]", canonical_map(m)),
        FalkorValue::Vec32(v) => {
            // A search vector's lane order is meaningful — preserve it; bit-pattern each `f32`.
            let mut s = String::new();
            for f in &v.values {
                push_field(&mut s, &format!("{:08x}", f.to_bits()));
            }
            format!("vec32[{s}]")
        }
        FalkorValue::Point(p) => format!(
            "point:{:016x},{:016x}",
            p.latitude.to_bits(),
            p.longitude.to_bits()
        ),
        FalkorValue::DateTime(t) => format!("datetime:{}", t.seconds().get()),
        FalkorValue::Date(t) => format!("date:{}", t.seconds().get()),
        FalkorValue::Time(t) => format!("time:{}", t.seconds().get()),
        FalkorValue::Duration(t) => format!("duration:{}", t.seconds().get()),
        // `FalkorValue` is `#[non_exhaustive]`; a future variant falls back to `Debug` (tagged `o:`)
        // so the digest stays defined rather than failing to compile.
        other => format!("o:{other:?}"),
    }
}

/// Append `content` to `out` length-prefixed as `<byte-len>:<content>`, so concatenated fields can't
/// shift a boundary and alias a different structure (the same framing [`canonical_row`] uses).
fn push_field(
    out: &mut String,
    content: &str,
) {
    out.push_str(&content.len().to_string());
    out.push(':');
    out.push_str(content);
}

/// Canonicalize a property/`Map` bag: entries emitted **key-sorted** (a `HashMap`'s iteration order
/// is process-unstable) as length-prefixed `key`/`value` pairs.
fn canonical_map(m: &HashMap<String, FalkorValue>) -> String {
    let mut entries: Vec<(&String, &FalkorValue)> = m.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut s = String::new();
    for (k, val) in entries {
        push_field(&mut s, k);
        push_field(&mut s, &canonical_value(val));
    }
    s
}

/// Canonicalize a [`Node`]: its id, its labels (server order preserved) and its key-sorted
/// properties.
fn canonical_node(n: &Node) -> String {
    let mut labels = String::new();
    for l in &n.labels {
        push_field(&mut labels, l);
    }
    let mut s = String::new();
    push_field(&mut s, &format!("id:{}", n.entity_id));
    push_field(&mut s, &labels);
    push_field(&mut s, &canonical_map(&n.properties));
    format!("node[{s}]")
}

/// Canonicalize an [`Edge`]: its id, relationship type, endpoints and key-sorted properties.
fn canonical_edge(e: &Edge) -> String {
    let mut s = String::new();
    push_field(&mut s, &format!("id:{}", e.entity_id));
    push_field(&mut s, &format!("type:{}", e.relationship_type));
    push_field(&mut s, &format!("src:{}", e.src_node_id));
    push_field(&mut s, &format!("dst:{}", e.dst_node_id));
    push_field(&mut s, &canonical_map(&e.properties));
    format!("edge[{s}]")
}

/// Validate FalkorDB's reported internal execution time: it must be present, finite and
/// non-negative. A missing or garbage statistic becomes a hard error rather than poisoning the
/// summary with a `NaN`/`0`.
fn validate_server_ms(
    reported: Option<f64>,
    cypher: &str,
) -> BenchmarkResult<f64> {
    let server_ms = reported.ok_or_else(|| {
        OtherError(format!(
            "response for '{}' had no internal execution time statistic",
            cypher
        ))
    })?;
    if !server_ms.is_finite() || server_ms < 0.0 {
        return Err(OtherError(format!(
            "response for '{}' reported an invalid internal execution time: {}",
            cypher, server_ms
        )));
    }
    Ok(server_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use falkordb::{Date, DateTime, Duration, Path, Point, Time};

    fn map_of(pairs: &[(&str, FalkorValue)]) -> HashMap<String, FalkorValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn canonical_value_covers_every_scalar_shape() {
        assert_eq!(canonical_value(&FalkorValue::I64(7)), "i:7");
        assert_eq!(canonical_value(&FalkorValue::Bool(true)), "b:true");
        assert_eq!(canonical_value(&FalkorValue::String("x".into())), "s:x");
        assert_eq!(canonical_value(&FalkorValue::None), "null");
        assert_eq!(
            canonical_value(&FalkorValue::Unparseable("boom".into())),
            "unparseable:boom"
        );
        // Floats use their bit pattern so equal values render identically.
        assert_eq!(
            canonical_value(&FalkorValue::F64(1.5)),
            canonical_value(&FalkorValue::F64(1.5))
        );
        assert!(canonical_value(&FalkorValue::F64(1.5)).starts_with("f:"));
    }

    #[test]
    fn canonical_value_renders_temporal_and_point() {
        assert_eq!(
            canonical_value(&FalkorValue::DateTime(DateTime::new(90))),
            "datetime:90"
        );
        assert_eq!(
            canonical_value(&FalkorValue::Date(Date::new(-5))),
            "date:-5"
        );
        assert_eq!(canonical_value(&FalkorValue::Time(Time::new(3))), "time:3");
        assert_eq!(
            canonical_value(&FalkorValue::Duration(Duration::new(60))),
            "duration:60"
        );
        // A point renders both coordinates by bit pattern; equal points render identically.
        let p = FalkorValue::Point(Point {
            latitude: 1.5,
            longitude: -2.25,
        });
        assert!(canonical_value(&p).starts_with("point:"));
        assert_eq!(
            canonical_value(&p),
            canonical_value(&FalkorValue::Point(Point {
                latitude: 1.5,
                longitude: -2.25,
            }))
        );
    }

    #[test]
    fn canonical_map_sorts_keys_regardless_of_insertion_order() {
        // Two maps with identical entries but built in the opposite insertion order canonicalize
        // identically — the whole point, since a `HashMap`'s iteration order is process-unstable.
        let mut a = HashMap::new();
        a.insert("b".to_string(), FalkorValue::I64(2));
        a.insert("a".to_string(), FalkorValue::I64(1));
        let mut b = HashMap::new();
        b.insert("a".to_string(), FalkorValue::I64(1));
        b.insert("b".to_string(), FalkorValue::I64(2));
        assert_eq!(canonical_map(&a), canonical_map(&b));
        // …and the canonical order is key-sorted ("a" before "b"), length-prefixed.
        assert_eq!(canonical_map(&a), "1:a3:i:11:b3:i:2");
        assert_eq!(
            canonical_value(&FalkorValue::Map(a)),
            "map[1:a3:i:11:b3:i:2]"
        );
    }

    #[test]
    fn canonical_map_is_value_sensitive() {
        let a = FalkorValue::Map(map_of(&[("k", FalkorValue::I64(1))]));
        let b = FalkorValue::Map(map_of(&[("k", FalkorValue::I64(2))]));
        assert_ne!(canonical_value(&a), canonical_value(&b));
        // A different key changes it too.
        let c = FalkorValue::Map(map_of(&[("j", FalkorValue::I64(1))]));
        assert_ne!(canonical_value(&a), canonical_value(&c));
    }

    #[test]
    fn canonical_value_preserves_array_order() {
        let asc = FalkorValue::Array(vec![FalkorValue::I64(1), FalkorValue::I64(2)]);
        let desc = FalkorValue::Array(vec![FalkorValue::I64(2), FalkorValue::I64(1)]);
        assert!(canonical_value(&asc).starts_with("arr["));
        // Order is meaningful for arrays, so a reordering must change the canonical form.
        assert_ne!(canonical_value(&asc), canonical_value(&desc));
    }

    #[test]
    fn canonical_value_recurses_into_nested_containers() {
        // A map containing an array containing a node: insertion order of the map still doesn't
        // matter, but the nested array order and node identity do.
        let node = FalkorValue::Node(Node {
            entity_id: 7,
            labels: vec!["User".into()],
            properties: map_of(&[("id", FalkorValue::I64(7))]),
        });
        let inner = FalkorValue::Array(vec![node, FalkorValue::I64(9)]);
        let one = FalkorValue::Map(map_of(&[
            ("z", FalkorValue::Bool(true)),
            ("items", inner.clone()),
        ]));
        let two = FalkorValue::Map(map_of(&[("items", inner), ("z", FalkorValue::Bool(true))]));
        assert_eq!(canonical_value(&one), canonical_value(&two));
    }

    #[test]
    fn canonical_node_sorts_properties_but_not_labels() {
        let forward = FalkorValue::Node(Node {
            entity_id: 1,
            labels: vec!["A".into(), "B".into()],
            properties: {
                let mut p = HashMap::new();
                p.insert("age".into(), FalkorValue::I64(30));
                p.insert("name".into(), FalkorValue::String("Alice".into()));
                p
            },
        });
        let props_reversed = FalkorValue::Node(Node {
            entity_id: 1,
            labels: vec!["A".into(), "B".into()],
            properties: {
                let mut p = HashMap::new();
                p.insert("name".into(), FalkorValue::String("Alice".into()));
                p.insert("age".into(), FalkorValue::I64(30));
                p
            },
        });
        // Property insertion order doesn't matter (keys are sorted).
        assert_eq!(canonical_value(&forward), canonical_value(&props_reversed));
        assert!(canonical_value(&forward).starts_with("node["));
        // Label order *is* preserved, so swapping labels changes the canonical form.
        let labels_swapped = FalkorValue::Node(Node {
            entity_id: 1,
            labels: vec!["B".into(), "A".into()],
            properties: HashMap::new(),
        });
        let labels_forward = FalkorValue::Node(Node {
            entity_id: 1,
            labels: vec!["A".into(), "B".into()],
            properties: HashMap::new(),
        });
        assert_ne!(
            canonical_value(&labels_swapped),
            canonical_value(&labels_forward)
        );
    }

    #[test]
    fn canonical_edge_sorts_properties_and_captures_endpoints() {
        let edge = |props: HashMap<String, FalkorValue>| {
            FalkorValue::Edge(Edge {
                entity_id: 5,
                relationship_type: "KNOWS".into(),
                src_node_id: 1,
                dst_node_id: 2,
                properties: props,
            })
        };
        let mut p1 = HashMap::new();
        p1.insert("since".into(), FalkorValue::I64(2020));
        p1.insert("weight".into(), FalkorValue::F64(0.5));
        let mut p2 = HashMap::new();
        p2.insert("weight".into(), FalkorValue::F64(0.5));
        p2.insert("since".into(), FalkorValue::I64(2020));
        assert_eq!(canonical_value(&edge(p1)), canonical_value(&edge(p2)));
        assert!(canonical_value(&edge(HashMap::new())).starts_with("edge["));
        // A different endpoint changes the canonical form.
        let flipped = FalkorValue::Edge(Edge {
            entity_id: 5,
            relationship_type: "KNOWS".into(),
            src_node_id: 2,
            dst_node_id: 1,
            properties: HashMap::new(),
        });
        assert_ne!(
            canonical_value(&edge(HashMap::new())),
            canonical_value(&flipped)
        );
    }

    #[test]
    fn canonical_path_preserves_node_and_edge_order() {
        let node = |id: i64| Node {
            entity_id: id,
            labels: vec!["User".into()],
            properties: HashMap::new(),
        };
        let edge = |id: i64, src: i64, dst: i64| Edge {
            entity_id: id,
            relationship_type: "KNOWS".into(),
            src_node_id: src,
            dst_node_id: dst,
            properties: HashMap::new(),
        };
        let forward = FalkorValue::Path(Path {
            nodes: vec![node(1), node(2)],
            relationships: vec![edge(10, 1, 2)],
        });
        assert!(canonical_value(&forward).starts_with("path["));
        // Reversing the node order along the path changes the canonical form.
        let reversed = FalkorValue::Path(Path {
            nodes: vec![node(2), node(1)],
            relationships: vec![edge(10, 1, 2)],
        });
        assert_ne!(canonical_value(&forward), canonical_value(&reversed));
    }

    #[test]
    fn canonical_value_distinguishes_structurally_different_shapes() {
        // A string that happens to look like a compound's rendering can't alias the real compound
        // (distinct type tags + length framing).
        let arr = FalkorValue::Array(vec![FalkorValue::I64(1)]);
        let lookalike = FalkorValue::String(canonical_value(&arr).trim_start_matches("arr").into());
        assert_ne!(canonical_value(&arr), canonical_value(&lookalike));
        // A node and an edge with the same id don't collide.
        let node = FalkorValue::Node(Node {
            entity_id: 1,
            labels: vec![],
            properties: HashMap::new(),
        });
        let edge = FalkorValue::Edge(Edge {
            entity_id: 1,
            relationship_type: String::new(),
            src_node_id: 0,
            dst_node_id: 0,
            properties: HashMap::new(),
        });
        assert_ne!(canonical_value(&node), canonical_value(&edge));
        // An array and a map don't collide even when "empty".
        assert_ne!(
            canonical_value(&FalkorValue::Array(vec![])),
            canonical_value(&FalkorValue::Map(HashMap::new()))
        );
    }

    #[test]
    fn canonical_row_joins_columns_stably() {
        let row = vec![FalkorValue::I64(3), FalkorValue::String("a".into())];
        let s = canonical_row(&row);
        assert!(s.contains("i:3") && s.contains("s:a"));
        // Column order is preserved (a different order ⇒ different string).
        let swapped = vec![FalkorValue::String("a".into()), FalkorValue::I64(3)];
        assert_ne!(canonical_row(&row), canonical_row(&swapped));
    }

    #[test]
    fn validate_server_ms_accepts_finite_non_negative() {
        assert_eq!(validate_server_ms(Some(0.0), "q").unwrap(), 0.0);
        assert_eq!(validate_server_ms(Some(1.5), "q").unwrap(), 1.5);
    }

    #[test]
    fn validate_server_ms_rejects_missing_and_invalid() {
        // Absent statistic.
        assert!(validate_server_ms(None, "q").is_err());
        // Non-finite and negative values are both rejected.
        assert!(validate_server_ms(Some(f64::NAN), "q").is_err());
        assert!(validate_server_ms(Some(f64::INFINITY), "q").is_err());
        assert!(validate_server_ms(Some(-0.001), "q").is_err());
    }
}
