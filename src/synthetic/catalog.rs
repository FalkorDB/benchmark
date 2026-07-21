//! The read-operation catalog: the corpus of Cypher operations the probe can measure, plus the
//! minimal dataset handle their parameter corpora are drawn from.
//!
//! Each [`OpName`] maps to exactly one [`OperationSpec`] via [`spec`] (an exhaustive match, so a
//! new variant won't compile until it has a corpus). A spec's [`CorpusFn`] pre-generates a fixed
//! set of *parameterized* queries from a seeded RNG and a [`DatasetHandle`], so the same seed
//! yields a byte-identical corpus (determinism is unit-tested). Every query projects **scalars**
//! (never whole nodes/edges) so draining a row never triggers FalkorDB schema-resolution
//! round-trips on the single probe connection.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::queries_repository::QueryType;
use crate::query::{Query, QueryBuilder};
use crate::synthetic::writes::{ExpectedMutation, WritePlan, WriteScratch};
use crate::synthetic::OpName;
use rand::{Rng, RngExt};

/// Number of distinct parameterizations pre-generated per operation. The measured loop cycles
/// through this corpus, so varying parameter *values* exercise real binding while the query
/// **body** stays constant (keeping the plan cache warm in cached mode).
pub const CORPUS_SIZE: usize = 256;

/// A minimal, seed-independent snapshot of the live graph that operation corpora draw from.
///
/// In Part 2 this is sampled from whatever graph the endpoint already has; Part 3's generator
/// ([`crate::synthetic::dataset`]) builds it directly from a seeded [`DatasetSpec`]. `node_ids` is
/// sorted ascending for a stable sample; `connected_pairs` holds `(from, to)` pairs known to be
/// reachable within the bounded `shortest_path` hop limit (empty when sampled from an external
/// graph whose connectivity we can't assume).
///
/// [`DatasetSpec`]: crate::synthetic::dataset::DatasetSpec
#[derive(Debug, Clone, Default)]
pub struct DatasetHandle {
    /// A sample of existing `:User` ids, ascending. Empty if the graph has no `:User` nodes.
    pub node_ids: Vec<i32>,
    /// Seeded `(from, to)` pairs guaranteed connected within the shortest-path hop bound. Empty for
    /// externally-probed graphs (Part 2), populated by the Part 3 generator.
    pub connected_pairs: Vec<(i32, i32)>,
}

impl DatasetHandle {
    /// The sampled ids, or a clear error naming the op that needs seed data when the graph has
    /// none (so a user pointed at an empty/foreign graph gets an actionable message instead of a
    /// panic).
    fn ids(
        &self,
        op: &str,
        need: usize,
    ) -> BenchmarkResult<&[i32]> {
        if self.node_ids.len() < need {
            return Err(OtherError(format!(
                "operation '{}' needs at least {} seed :User id(s) but the graph sample has {} — \
                 load a dataset (see Part 3) or point --graph at a populated graph",
                op,
                need,
                self.node_ids.len()
            )));
        }
        Ok(&self.node_ids)
    }
}

/// What seed data an operation's corpus requires from the [`DatasetHandle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetRequirement {
    /// Needs no seed ids (self-contained or full-scan).
    None,
    /// Needs at least one existing `:User` id.
    OneId,
    /// Needs at least two existing `:User` ids (e.g. shortest-path endpoints).
    TwoIds,
}

impl DatasetRequirement {
    fn min_ids(self) -> usize {
        match self {
            DatasetRequirement::None => 0,
            DatasetRequirement::OneId => 1,
            DatasetRequirement::TwoIds => 2,
        }
    }
}

/// Object-safe corpus builder: given a seeded RNG, the dataset sample, and this worker's index of
/// `workers` total (both `0`/`1` at concurrency 1; reserved for the Part 4 concurrency sweep), it
/// returns a fixed-size vector of parameterized queries that all share one query body.
pub type CorpusFn =
    fn(&mut dyn Rng, &DatasetHandle, usize, usize) -> BenchmarkResult<Vec<Query>>;

/// One catalog entry: an operation's identity, its read/write kind, a one-line description, the
/// seed data it needs, and its corpus builder. `OpName` stays the single source of truth; this
/// spec is what the runner and `--help`/completion all agree on.
#[derive(Clone, Copy)]
pub struct OperationSpec {
    pub name: OpName,
    pub kind: QueryType,
    pub description: &'static str,
    pub requirement: DatasetRequirement,
    pub corpus: CorpusFn,
    /// Present for write operations (Part 5): the lifecycle hooks, mutation to verify, and timed
    /// query builder. `None` for reads, which use `corpus` instead.
    pub write: Option<WritePlan>,
}

impl OperationSpec {
    /// Build this operation's parameter corpus, first validating that the dataset satisfies the
    /// operation's [`DatasetRequirement`] so the error names the op rather than panicking deep in
    /// a corpus closure.
    pub fn build_corpus(
        &self,
        rng: &mut dyn Rng,
        dataset: &DatasetHandle,
        worker: usize,
        workers: usize,
    ) -> BenchmarkResult<Vec<Query>> {
        let _ = dataset.ids(self.name.as_str(), self.requirement.min_ids())?;
        let corpus = (self.corpus)(rng, dataset, worker, workers)?;
        if corpus.is_empty() {
            return Err(OtherError(format!(
                "operation '{}' produced an empty corpus",
                self.name.as_str()
            )));
        }
        Ok(corpus)
    }
}

/// The full read-operation catalog, in `OpName` declaration order.
pub fn catalog() -> Vec<OperationSpec> {
    OpName::all().iter().map(|&op| spec(op)).collect()
}

/// The [`OperationSpec`] for one operation. Exhaustive over [`OpName`] — adding a variant forces a
/// corpus here.
pub fn spec(op: OpName) -> OperationSpec {
    match op {
        OpName::ReturnConst => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "RETURN $i — pure round-trip baseline (no dataset required)",
            requirement: DatasetRequirement::None,
            corpus: corpus_return_const,
            write: None,
        },
        OpName::MatchByIndex => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "point lookup on the :User(id) index",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_match_by_index,
            write: None,
        },
        OpName::MatchByLabelScan => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "full :User label scan with a non-indexable predicate",
            requirement: DatasetRequirement::None,
            corpus: corpus_match_by_label_scan,
            write: None,
        },
        OpName::Expand1Hop => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "1-hop :Friend expansion from a seed node",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_expand_1_hop,
            write: None,
        },
        OpName::ExpandHops5 => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "fixed 5-hop :Friend expansion from a seed node",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_expand_hops_5,
            write: None,
        },
        OpName::AggregateCount => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "count a seed node's 1-hop :Friend neighbours",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_aggregate_count,
            write: None,
        },
        OpName::AggregateGroup => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "group a seed node's neighbours by age with counts",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_aggregate_group,
            write: None,
        },
        OpName::ShortestPath => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "bounded shortest :Friend path between two seed nodes",
            requirement: DatasetRequirement::TwoIds,
            corpus: corpus_shortest_path,
            write: None,
        },
        OpName::PropertyProjection => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "project scalar properties of an indexed node",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_property_projection,
            write: None,
        },
        OpName::CreateNode => OperationSpec {
            name: op,
            kind: QueryType::Write,
            description: "create a fresh scratch node each invocation",
            requirement: DatasetRequirement::None,
            corpus: write_corpus_stub,
            write: Some(WritePlan {
                expected: ExpectedMutation::NodeCreated,
                plan_tag: "create_node.v1",
                default_reset_every: DEFAULT_RESET_EVERY,
                setup: write_clear_band,
                reset: write_clear_band,
                cleanup: write_cleanup_run,
                render: write_create_node_render,
            }),
        },
        OpName::MergeMiss => OperationSpec {
            name: op,
            kind: QueryType::Write,
            description: "MERGE a fresh scratch node each invocation (always misses → creates)",
            requirement: DatasetRequirement::None,
            corpus: write_corpus_stub,
            write: Some(WritePlan {
                expected: ExpectedMutation::NodeCreated,
                plan_tag: "merge_miss.v1",
                default_reset_every: DEFAULT_RESET_EVERY,
                setup: write_clear_band,
                reset: write_clear_band,
                cleanup: write_cleanup_run,
                render: write_merge_miss_render,
            }),
        },
        OpName::CreateEdge => OperationSpec {
            name: op,
            kind: QueryType::Write,
            description: "create a fresh edge between two of this worker's scratch nodes",
            requirement: DatasetRequirement::None,
            corpus: write_corpus_stub,
            write: Some(WritePlan {
                expected: ExpectedMutation::RelationshipCreated,
                plan_tag: "create_edge.v1",
                default_reset_every: DEFAULT_RESET_EVERY,
                setup: write_reset_populated,
                reset: write_reset_populated,
                cleanup: write_cleanup_run,
                render: write_create_edge_render,
            }),
        },
        OpName::SetProperty => OperationSpec {
            name: op,
            kind: QueryType::Write,
            description: "set a property on a pre-created scratch node each invocation",
            requirement: DatasetRequirement::None,
            corpus: write_corpus_stub,
            write: Some(WritePlan {
                expected: ExpectedMutation::PropertySet,
                plan_tag: "set_property.v1",
                default_reset_every: DEFAULT_RESET_EVERY,
                setup: write_reset_populated,
                reset: write_reset_populated,
                cleanup: write_cleanup_run,
                render: write_set_property_render,
            }),
        },
        OpName::DeleteNode => OperationSpec {
            name: op,
            kind: QueryType::Write,
            description: "delete a pre-created scratch node each invocation",
            requirement: DatasetRequirement::None,
            corpus: write_corpus_stub,
            write: Some(WritePlan {
                expected: ExpectedMutation::NodeDeleted,
                plan_tag: "delete_node.v1",
                default_reset_every: DEFAULT_RESET_EVERY,
                setup: write_reset_populated,
                reset: write_reset_populated,
                cleanup: write_cleanup_run,
                render: write_delete_node_render,
            }),
        },
        OpName::MergeHit => OperationSpec {
            name: op,
            kind: QueryType::Write,
            description: "MERGE an existing scratch node each invocation (always hits)",
            requirement: DatasetRequirement::None,
            corpus: write_corpus_stub,
            write: Some(WritePlan {
                expected: ExpectedMutation::NodeMatched,
                plan_tag: "merge_hit.v1",
                default_reset_every: DEFAULT_RESET_EVERY,
                setup: write_reset_populated,
                reset: write_noop,
                cleanup: write_cleanup_run,
                render: write_merge_hit_render,
            }),
        },
    }
}

/// Default reset cadence (measured ops per sawtooth window) for write operations.
pub const DEFAULT_RESET_EVERY: usize = 50_000;

/// Delete only this worker's scratch rows (its key band) — used as both **setup** (a clean start,
/// self-healing over any stale rows in this run's namespace) and **reset** (undo a window's
/// accumulation). Scoped by the run-unique label *and* the worker's id band, so it can never touch
/// another worker's or another run's data. `DETACH DELETE` so it also drops any edges an op left on
/// its band nodes (e.g. `create_edge`); harmless for the edgeless ops.
fn write_clear_band(scratch: &WriteScratch) -> BenchmarkResult<Vec<Query>> {
    let (lo, hi) = scratch.key_band();
    let text = format!(
        "MATCH (n:{}) WHERE n.id >= $lo AND n.id <= $hi DETACH DELETE n",
        scratch.label()
    );
    Ok(vec![QueryBuilder::new()
        .text(text)
        .param("lo", lo)
        .param("hi", hi)
        .build()])
}

/// Drop the whole run's scratch (every worker's rows for the run-unique label) — the coordinator
/// runs this once after a level, on a fresh connection. `DETACH DELETE` so a run that created edges
/// (`create_edge`) is fully cleared, edges included.
fn write_cleanup_run(scratch: &WriteScratch) -> BenchmarkResult<Vec<Query>> {
    let text = format!("MATCH (n:{}) DETACH DELETE n", scratch.label());
    Ok(vec![QueryBuilder::new().text(text).build()])
}

/// `create_node`: create a fresh scratch node with this invocation's within-window-unique id.
fn write_create_node_render(
    scratch: &WriteScratch,
    seq: u64,
) -> BenchmarkResult<Query> {
    let text = format!("CREATE (n:{} {{id: $id}}) RETURN n.id", scratch.label());
    Ok(QueryBuilder::new()
        .text(text)
        .param("id", scratch.window_key(seq))
        .build())
}

/// `merge_miss`: `MERGE` a scratch node whose id is unique within the window, so it always misses
/// and creates (never hits).
fn write_merge_miss_render(
    scratch: &WriteScratch,
    seq: u64,
) -> BenchmarkResult<Query> {
    let text = format!("MERGE (n:{} {{id: $id}}) RETURN n.id", scratch.label());
    Ok(QueryBuilder::new()
        .text(text)
        .param("id", scratch.window_key(seq))
        .build())
}

/// Create this worker's band nodes (`id` `lo..=hi`, one per key) so ops that need existing targets
/// (`delete_node`, `set_property`, `merge_hit`, `create_edge`) have a full, clean band. Precondition:
/// the band is empty (it is paired with [`write_clear_band`] in [`write_reset_populated`]).
fn write_fill_band(scratch: &WriteScratch) -> BenchmarkResult<Vec<Query>> {
    let (lo, hi) = scratch.key_band();
    let text = format!(
        "UNWIND range($lo, $hi) AS i CREATE (:{} {{id: i}})",
        scratch.label()
    );
    Ok(vec![QueryBuilder::new()
        .text(text)
        .param("lo", lo)
        .param("hi", hi)
        .build()])
}

/// Reset (and initial setup) for write ops that consume or mutate a **populated** band: clear the
/// worker's band (dropping any nodes + edges the window accumulated) then refill it with `R` fresh
/// clean nodes, so every window starts from an identical clean state. Bounds drift to one sawtooth
/// window exactly like the empty-band ops, just around a full band instead of an empty one.
fn write_reset_populated(scratch: &WriteScratch) -> BenchmarkResult<Vec<Query>> {
    let mut stmts = write_clear_band(scratch)?;
    stmts.extend(write_fill_band(scratch)?);
    Ok(stmts)
}

/// A no-op reset for a drift-free op (`merge_hit` only matches, never mutates): its band is set up
/// once and never needs refreshing, so refilling `R` nodes every window would be pure waste.
fn write_noop(_scratch: &WriteScratch) -> BenchmarkResult<Vec<Query>> {
    Ok(vec![])
}

/// `delete_node`: delete this invocation's pre-created band node — exactly one (`window_key` is
/// unique within a window, so each of the window's `R` nodes is deleted once, then the reset
/// refills the band).
fn write_delete_node_render(
    scratch: &WriteScratch,
    seq: u64,
) -> BenchmarkResult<Query> {
    let text = format!("MATCH (n:{} {{id: $id}}) DELETE n", scratch.label());
    Ok(QueryBuilder::new()
        .text(text)
        .param("id", scratch.window_key(seq))
        .build())
}

/// `set_property`: set one property on a pre-created band node. `WHERE n.touched IS NULL` makes the
/// sample self-checking — the band is refilled each window with `touched`-less nodes, so a genuine
/// first-write always sets exactly one property; if a broken reset left `touched` set, the match
/// finds nothing, zero properties are set, and mutation verification fails loudly.
fn write_set_property_render(
    scratch: &WriteScratch,
    seq: u64,
) -> BenchmarkResult<Query> {
    let text = format!(
        "MATCH (n:{} {{id: $id}}) WHERE n.touched IS NULL SET n.touched = $id",
        scratch.label()
    );
    Ok(QueryBuilder::new()
        .text(text)
        .param("id", scratch.window_key(seq))
        .build())
}

/// `merge_hit`: `MERGE` a band node that was pre-created in setup, so it always matches (never
/// creates). The band is populated once and never mutated, so no reset is needed.
fn write_merge_hit_render(
    scratch: &WriteScratch,
    seq: u64,
) -> BenchmarkResult<Query> {
    let text = format!("MERGE (n:{} {{id: $id}}) RETURN n.id", scratch.label());
    Ok(QueryBuilder::new()
        .text(text)
        .param("id", scratch.window_key(seq))
        .build())
}

/// `create_edge`: create one fresh edge between two distinct band nodes (`src → src+1`, wrapping the
/// band's top back to its bottom). Over a window the `R` sources yield `R` distinct ordered pairs —
/// no duplicate edges — and the reset drops the accumulated edges by clearing+refilling the band.
/// With `R == 1` the single node gets a self-loop. Endpoints stay inside this worker's band, so a
/// reset's `DETACH DELETE` never touches another worker's data.
fn write_create_edge_render(
    scratch: &WriteScratch,
    seq: u64,
) -> BenchmarkResult<Query> {
    let (lo, hi) = scratch.key_band();
    let src = scratch.window_key(seq);
    // Wrap the top of the band back to the bottom without ever computing `R` as i32 (R may exceed
    // i32 while every band key still fits it).
    let dst = if src == hi { lo } else { src + 1 };
    let text = format!(
        "MATCH (a:{l} {{id: $src}}), (b:{l} {{id: $dst}}) CREATE (a)-[:BenchEdge]->(b)",
        l = scratch.label()
    );
    Ok(QueryBuilder::new()
        .text(text)
        .param("src", src)
        .param("dst", dst)
        .build())
}

/// Placeholder corpus for write ops: the runner builds their queries per-invocation via the
/// [`WritePlan`] render, so this is never measured — it exists only to satisfy [`OperationSpec`]'s
/// `corpus` field and the non-empty-corpus invariant.
fn write_corpus_stub(
    _rng: &mut dyn Rng,
    _dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    Ok(vec![QueryBuilder::new().text("RETURN 1").build()])
}

/// Pick a uniformly-random id from the sample.
fn pick_id(
    rng: &mut dyn Rng,
    ids: &[i32],
) -> i32 {
    ids[rng.random_range(0..ids.len())]
}

/// Build `CORPUS_SIZE` queries that share one body, parameterized by one random seed id each.
fn one_id_corpus(
    rng: &mut dyn Rng,
    ids: &[i32],
    text: &'static str,
) -> Vec<Query> {
    (0..CORPUS_SIZE)
        .map(|_| {
            QueryBuilder::new()
                .text(text)
                .param("id", pick_id(rng, ids))
                .build()
        })
        .collect()
}

fn corpus_return_const(
    rng: &mut dyn Rng,
    _dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    Ok((0..CORPUS_SIZE)
        .map(|_| {
            QueryBuilder::new()
                .text("RETURN $i AS x")
                .param("i", rng.random_range(0..i32::MAX))
                .build()
        })
        .collect())
}

fn corpus_match_by_index(
    rng: &mut dyn Rng,
    dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    let ids = &dataset.node_ids;
    Ok(one_id_corpus(
        rng,
        ids,
        "MATCH (n:User {id: $id}) RETURN n.id",
    ))
}

fn corpus_match_by_label_scan(
    rng: &mut dyn Rng,
    _dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    // `n.id % $modulus` is a computed predicate the id index can't satisfy, forcing a full label
    // scan. `count(n)` keeps the payload one row while still doing the server-side scan work.
    Ok((0..CORPUS_SIZE)
        .map(|_| {
            QueryBuilder::new()
                .text("MATCH (n:User) WHERE n.id % $modulus = 0 RETURN count(n) AS c")
                .param("modulus", rng.random_range(2..=97))
                .build()
        })
        .collect())
}

fn corpus_expand_1_hop(
    rng: &mut dyn Rng,
    dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    let ids = &dataset.node_ids;
    Ok(one_id_corpus(
        rng,
        ids,
        "MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN n.id",
    ))
}

fn corpus_expand_hops_5(
    rng: &mut dyn Rng,
    dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    let ids = &dataset.node_ids;
    // Exactly five typed hops; `DISTINCT` + `LIMIT` bound the fan-out payload.
    Ok(one_id_corpus(
        rng,
        ids,
        "MATCH (s:User {id: $id})-[:Friend*5..5]->(n:User) RETURN DISTINCT n.id LIMIT 100",
    ))
}

fn corpus_aggregate_count(
    rng: &mut dyn Rng,
    dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    let ids = &dataset.node_ids;
    Ok(one_id_corpus(
        rng,
        ids,
        "MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN count(n) AS c",
    ))
}

fn corpus_aggregate_group(
    rng: &mut dyn Rng,
    dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    let ids = &dataset.node_ids;
    Ok(one_id_corpus(
        rng,
        ids,
        "MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN n.age AS age, count(*) AS c ORDER BY c DESC LIMIT 10",
    ))
}

fn corpus_shortest_path(
    rng: &mut dyn Rng,
    dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    // FalkorDB's shortestPath form is `WITH shortestPath(...) AS p` and requires a *directed*
    // pattern; bound the search to 6 hops and `coalesce` a missing path to -1 so an unreachable
    // pair returns a row instead of erroring.
    let text = "MATCH (s:User {id: $from}), (t:User {id: $to}) \
                WITH shortestPath((s)-[:Friend*1..6]->(t)) AS p RETURN coalesce(length(p), -1) AS len";

    // Prefer the generator's connected pairs (guaranteed to have a bounded path, so we measure real
    // path-finding rather than mostly-unreachable misses). Fall back to two distinct sampled ids
    // for an externally-probed graph whose connectivity we can't assume.
    if !dataset.connected_pairs.is_empty() {
        let pairs = &dataset.connected_pairs;
        return Ok((0..CORPUS_SIZE)
            .map(|_| {
                let (from, to) = pairs[rng.random_range(0..pairs.len())];
                QueryBuilder::new()
                    .text(text)
                    .param("from", from)
                    .param("to", to)
                    .build()
            })
            .collect());
    }

    let ids = &dataset.node_ids;
    Ok((0..CORPUS_SIZE)
        .map(|_| {
            // Pick two *distinct* indices (ids are unique) so `from != to`: choose `to` from the
            // n-1 other slots and skip past `from`.
            let n = ids.len();
            let from_idx = rng.random_range(0..n);
            let mut to_idx = rng.random_range(0..n - 1);
            if to_idx >= from_idx {
                to_idx += 1;
            }
            QueryBuilder::new()
                .text(text)
                .param("from", ids[from_idx])
                .param("to", ids[to_idx])
                .build()
        })
        .collect())
}

fn corpus_property_projection(
    rng: &mut dyn Rng,
    dataset: &DatasetHandle,
    _worker: usize,
    _workers: usize,
) -> BenchmarkResult<Vec<Query>> {
    let ids = &dataset.node_ids;
    Ok(one_id_corpus(
        rng,
        ids,
        "MATCH (n:User {id: $id}) RETURN n.id, n.age",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn catalog_covers_every_op_with_matching_spec() {
        let all = catalog();
        assert_eq!(all.len(), OpName::all().len());
        for (entry, op) in all.iter().zip(OpName::all()) {
            assert_eq!(entry.name, *op);
            // A write op carries a WritePlan and QueryType::Write; a read carries neither.
            assert_eq!(
                entry.kind == QueryType::Write,
                entry.write.is_some(),
                "op {} kind must match presence of a write plan",
                op.as_str()
            );
            assert!(!entry.description.is_empty());
        }
    }

    #[test]
    fn dataset_requirement_min_ids() {
        assert_eq!(DatasetRequirement::None.min_ids(), 0);
        assert_eq!(DatasetRequirement::OneId.min_ids(), 1);
        assert_eq!(DatasetRequirement::TwoIds.min_ids(), 2);
    }

    #[test]
    fn shortest_path_corpus_uses_two_distinct_params() {
        // The tightest case: exactly two ids and no connected-pair pool (external-graph path).
        let ds = DatasetHandle {
            node_ids: vec![1, 2],
            ..Default::default()
        };
        let mut rng = StdRng::seed_from_u64(9);
        let corpus = spec(OpName::ShortestPath)
            .build_corpus(&mut rng, &ds, 0, 1)
            .expect("shortest_path corpus");
        assert_eq!(corpus.len(), CORPUS_SIZE);
        for q in &corpus {
            assert!(q.text.contains("shortestPath"));
            let from = q.params.get("from").expect("from param");
            let to = q.params.get("to").expect("to param");
            assert_ne!(
                format!("{from:?}"),
                format!("{to:?}"),
                "shortest_path endpoints must be distinct"
            );
        }
    }

    #[test]
    fn shortest_path_prefers_connected_pairs_when_present() {
        use crate::query::QueryParam;
        // With a connected-pair pool, every query must draw its (from, to) from that pool.
        let ds = DatasetHandle {
            node_ids: (1..=1000).collect(),
            connected_pairs: vec![(10, 11), (20, 25), (30, 33)],
        };
        let allowed: std::collections::HashSet<(i32, i32)> =
            ds.connected_pairs.iter().copied().collect();
        let mut rng = StdRng::seed_from_u64(5);
        let corpus = spec(OpName::ShortestPath)
            .build_corpus(&mut rng, &ds, 0, 1)
            .expect("shortest_path corpus");
        for q in &corpus {
            let from = match q.params.get("from") {
                Some(QueryParam::Integer(i)) => *i,
                other => panic!("from not an integer: {other:?}"),
            };
            let to = match q.params.get("to") {
                Some(QueryParam::Integer(i)) => *i,
                other => panic!("to not an integer: {other:?}"),
            };
            assert!(
                allowed.contains(&(from, to)),
                "pair ({from},{to}) not from the connected-pair pool"
            );
        }
    }

    // ---- Part 5 write operations ----------------------------------------------------------------

    #[test]
    fn write_ops_carry_a_write_plan_and_write_kind() {
        use ExpectedMutation::*;
        let expected = [
            (OpName::CreateNode, NodeCreated),
            (OpName::MergeMiss, NodeCreated),
            (OpName::CreateEdge, RelationshipCreated),
            (OpName::SetProperty, PropertySet),
            (OpName::DeleteNode, NodeDeleted),
            (OpName::MergeHit, NodeMatched),
        ];
        for (op, mutation) in expected {
            let s = spec(op);
            assert_eq!(s.kind, QueryType::Write, "{} is a write op", op.as_str());
            let plan = s.write.expect("write op carries a WritePlan");
            assert_eq!(plan.expected, mutation, "{} expected mutation", op.as_str());
            assert_eq!(plan.default_reset_every, DEFAULT_RESET_EVERY);
        }
        // Every catalog op agrees on kind vs. the presence of a write plan.
        assert!(OpName::all()
            .iter()
            .all(|op| { (spec(*op).kind == QueryType::Write) == spec(*op).write.is_some() }));
        let read = spec(OpName::MatchByIndex);
        assert!(read.write.is_none());
        assert_eq!(read.kind, QueryType::Read);
    }

    #[test]
    fn create_node_render_targets_the_run_label_and_window_key() {
        use crate::query::QueryParam;
        // worker 2, reset_every 10 ⇒ window_key(3) = 2*10 + 3%10 = 23.
        let scratch = WriteScratch::new(0xABCD, 2, 10).unwrap();
        let q = write_create_node_render(&scratch, 3).unwrap();
        assert_eq!(q.text, "CREATE (n:BenchScratch_abcd {id: $id}) RETURN n.id");
        assert!(
            matches!(q.params.get("id"), Some(QueryParam::Integer(23))),
            "id must be the worker's window key, got {:?}",
            q.params.get("id")
        );
    }

    #[test]
    fn merge_miss_render_uses_merge_with_the_window_key() {
        use crate::query::QueryParam;
        let scratch = WriteScratch::new(0x1, 0, 100).unwrap();
        let q = write_merge_miss_render(&scratch, 7).unwrap();
        assert_eq!(q.text, "MERGE (n:BenchScratch_1 {id: $id}) RETURN n.id");
        assert!(matches!(q.params.get("id"), Some(QueryParam::Integer(7))));
    }

    #[test]
    fn clear_band_deletes_only_this_workers_key_band() {
        use crate::query::QueryParam;
        // worker 3, reset_every 10 ⇒ band [30, 39].
        let scratch = WriteScratch::new(0xF, 3, 10).unwrap();
        let stmts = write_clear_band(&scratch).unwrap();
        assert_eq!(stmts.len(), 1);
        let q = &stmts[0];
        assert_eq!(
            q.text,
            "MATCH (n:BenchScratch_f) WHERE n.id >= $lo AND n.id <= $hi DETACH DELETE n"
        );
        assert!(matches!(q.params.get("lo"), Some(QueryParam::Integer(30))));
        assert!(matches!(q.params.get("hi"), Some(QueryParam::Integer(39))));
    }

    #[test]
    fn cleanup_run_drops_the_whole_run_label_unparameterized() {
        // Cleanup is scoped by the run-unique label (not a key band), so it wipes every worker's
        // rows at once and carries no params.
        let scratch = WriteScratch::new(0x2a, 5, 10).unwrap();
        let stmts = write_cleanup_run(&scratch).unwrap();
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].text, "MATCH (n:BenchScratch_2a) DETACH DELETE n");
        assert!(stmts[0].params.is_empty());
    }

    #[test]
    fn write_corpus_stub_is_a_single_placeholder_query() {
        // Writes render their measured query per-invocation from the WritePlan, so the corpus is a
        // never-measured stub that only satisfies the non-empty-corpus invariant.
        let mut rng = StdRng::seed_from_u64(1);
        let corpus = spec(OpName::CreateNode)
            .build_corpus(&mut rng, &DatasetHandle::default(), 0, 4)
            .expect("stub corpus builds without a dataset");
        assert_eq!(corpus.len(), 1);
    }

    #[test]
    fn delete_node_render_deletes_the_window_key_node() {
        use crate::query::QueryParam;
        let scratch = WriteScratch::new(0x7, 1, 10).unwrap(); // band [10, 19]
        let q = write_delete_node_render(&scratch, 4).unwrap(); // window_key = 10 + 4 = 14
        assert_eq!(q.text, "MATCH (n:BenchScratch_7 {id: $id}) DELETE n");
        assert!(matches!(q.params.get("id"), Some(QueryParam::Integer(14))));
    }

    #[test]
    fn set_property_render_guards_on_a_fresh_node() {
        use crate::query::QueryParam;
        // The `WHERE n.touched IS NULL` guard makes the sample self-checking: a broken reset that
        // left `touched` set would match nothing, set zero properties, and fail verification.
        let scratch = WriteScratch::new(0x8, 0, 100).unwrap();
        let q = write_set_property_render(&scratch, 5).unwrap();
        assert_eq!(
            q.text,
            "MATCH (n:BenchScratch_8 {id: $id}) WHERE n.touched IS NULL SET n.touched = $id"
        );
        assert!(matches!(q.params.get("id"), Some(QueryParam::Integer(5))));
    }

    #[test]
    fn merge_hit_render_merges_the_window_key_node() {
        use crate::query::QueryParam;
        let scratch = WriteScratch::new(0x9, 2, 10).unwrap(); // band [20, 29]
        let q = write_merge_hit_render(&scratch, 3).unwrap(); // window_key = 20 + 3 = 23
        assert_eq!(q.text, "MERGE (n:BenchScratch_9 {id: $id}) RETURN n.id");
        assert!(matches!(q.params.get("id"), Some(QueryParam::Integer(23))));
    }

    #[test]
    fn create_edge_render_connects_distinct_band_nodes_and_wraps_at_the_top() {
        use crate::query::QueryParam;
        let scratch = WriteScratch::new(0xA, 0, 5).unwrap(); // band [0, 4]
        // Mid-band: src = window_key(2) = 2 → dst = src + 1 = 3.
        let q = write_create_edge_render(&scratch, 2).unwrap();
        assert_eq!(
            q.text,
            "MATCH (a:BenchScratch_a {id: $src}), (b:BenchScratch_a {id: $dst}) CREATE (a)-[:BenchEdge]->(b)"
        );
        assert!(matches!(q.params.get("src"), Some(QueryParam::Integer(2))));
        assert!(matches!(q.params.get("dst"), Some(QueryParam::Integer(3))));
        // Top of the band wraps back to the bottom: src = hi = 4 → dst = lo = 0.
        let top = write_create_edge_render(&scratch, 4).unwrap();
        assert!(matches!(top.params.get("src"), Some(QueryParam::Integer(4))));
        assert!(matches!(top.params.get("dst"), Some(QueryParam::Integer(0))));
    }

    #[test]
    fn create_edge_self_loops_when_band_width_is_one() {
        use crate::query::QueryParam;
        let scratch = WriteScratch::new(0xB, 3, 1).unwrap(); // band [3, 3]
        let q = write_create_edge_render(&scratch, 9).unwrap(); // window_key = 3
        assert!(matches!(q.params.get("src"), Some(QueryParam::Integer(3))));
        assert!(matches!(q.params.get("dst"), Some(QueryParam::Integer(3))));
    }

    #[test]
    fn fill_band_creates_one_node_per_key_via_range() {
        use crate::query::QueryParam;
        let scratch = WriteScratch::new(0xC, 2, 10).unwrap(); // band [20, 29]
        let stmts = write_fill_band(&scratch).unwrap();
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0].text,
            "UNWIND range($lo, $hi) AS i CREATE (:BenchScratch_c {id: i})"
        );
        assert!(matches!(
            stmts[0].params.get("lo"),
            Some(QueryParam::Integer(20))
        ));
        assert!(matches!(
            stmts[0].params.get("hi"),
            Some(QueryParam::Integer(29))
        ));
    }

    #[test]
    fn reset_populated_clears_then_refills_the_band() {
        let scratch = WriteScratch::new(0xD, 0, 10).unwrap();
        let stmts = write_reset_populated(&scratch).unwrap();
        // Two statements, in order: DETACH DELETE the band, then recreate it clean.
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].text.contains("DETACH DELETE n"));
        assert!(stmts[1].text.contains("CREATE (:BenchScratch_d {id: i})"));
    }

    #[test]
    fn noop_reset_is_empty() {
        // merge_hit is drift-free, so its reset issues no statements.
        let scratch = WriteScratch::new(0xE, 0, 10).unwrap();
        assert!(write_noop(&scratch).unwrap().is_empty());
    }
}
