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
use crate::synthetic::OpName;
use rand::{Rng, RngExt};

/// Number of distinct parameterizations pre-generated per operation. The measured loop cycles
/// through this corpus, so varying parameter *values* exercise real binding while the query
/// **body** stays constant (keeping the plan cache warm in cached mode).
pub const CORPUS_SIZE: usize = 256;

/// A minimal, seed-independent snapshot of the live graph that operation corpora draw from.
///
/// Part 2 reads this from whatever graph the endpoint already has (Part 3 will generate a
/// reproducible dataset). `node_ids` is sorted ascending for a stable, reproducible sample.
#[derive(Debug, Clone, Default)]
pub struct DatasetHandle {
    /// A sample of existing `:User` ids, ascending. Empty if the graph has no `:User` nodes.
    pub node_ids: Vec<i32>,
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
        },
        OpName::MatchByIndex => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "point lookup on the :User(id) index",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_match_by_index,
        },
        OpName::MatchByLabelScan => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "full :User label scan with a non-indexable predicate",
            requirement: DatasetRequirement::None,
            corpus: corpus_match_by_label_scan,
        },
        OpName::Expand1Hop => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "1-hop :Friend expansion from a seed node",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_expand_1_hop,
        },
        OpName::ExpandHops5 => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "fixed 5-hop :Friend expansion from a seed node",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_expand_hops_5,
        },
        OpName::AggregateCount => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "count a seed node's 1-hop :Friend neighbours",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_aggregate_count,
        },
        OpName::AggregateGroup => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "group a seed node's neighbours by age with counts",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_aggregate_group,
        },
        OpName::ShortestPath => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "bounded shortest :Friend path between two seed nodes",
            requirement: DatasetRequirement::TwoIds,
            corpus: corpus_shortest_path,
        },
        OpName::PropertyProjection => OperationSpec {
            name: op,
            kind: QueryType::Read,
            description: "project scalar properties of an indexed node",
            requirement: DatasetRequirement::OneId,
            corpus: corpus_property_projection,
        },
    }
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
    let ids = &dataset.node_ids;
    // FalkorDB's shortestPath form is `WITH shortestPath(...) AS p` and requires a *directed*
    // pattern; bound the search to 6 hops and `coalesce` a missing path to -1 so an unreachable
    // pair returns a row instead of erroring.
    let text = "MATCH (s:User {id: $from}), (t:User {id: $to}) \
                WITH shortestPath((s)-[:Friend*1..6]->(t)) AS p RETURN coalesce(length(p), -1) AS len";
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
            assert_eq!(entry.kind, QueryType::Read, "Part 2 ops are all reads");
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
        // The tightest case: exactly two ids. Every entry must use both params with from != to.
        let ds = DatasetHandle {
            node_ids: vec![1, 2],
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
}
