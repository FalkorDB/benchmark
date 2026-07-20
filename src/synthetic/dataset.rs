//! Seeded, reproducible synthetic dataset generator (Part 3).
//!
//! Generates a deterministic `:User {id, age}` / `(:User)-[:Friend]->(:User)` graph from a
//! [`DatasetSpec`] and bulk-loads it via `UNWIND` batches, so operation numbers are controlled and
//! comparable across runs, machines and FalkorDB versions. All randomness is derived from a
//! portable [`splitmix64`] stream keyed by `(seed, domain, index)` — **not** `rand`'s `StdRng`,
//! whose output isn't guaranteed stable across versions — so "same seed ⇒ same dataset" holds
//! everywhere. A [`corpus_hash`] over the spec + selected operations + query bodies + sampled pools
//! is recorded in the report so runs are only compared when the workload truly matches.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::query::Query;
use crate::synthetic::catalog::DatasetHandle;
use crate::synthetic::OpName;
use falkordb::AsyncGraph;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::Duration;

/// Bumped whenever the generator algorithm or the operation catalog's query bodies change, so a
/// [`corpus_hash`] from an older build never compares equal to a newer, differently-generated one.
pub const GENERATOR_VERSION: &str = "synthbench/v2";

/// Max distinct `:User` ids sampled into the [`DatasetHandle`] id pool.
const POOL_IDS: usize = 4096;
/// Max connected `(from, to)` pairs sampled into the [`DatasetHandle`] pair pool.
const POOL_PAIRS: usize = 1024;
/// Longest ring distance used when building guaranteed-connected pairs (kept ≤ 5 so the bounded
/// `shortest_path` query — `[:Friend*1..6]` — always finds a path).
const MAX_PAIR_HOPS: usize = 5;

// Domain separators so independent derived streams (ages, edge endpoints, pools) never correlate.
const DOMAIN_AGE: u64 = 0x4147_45f0;
const DOMAIN_EDGE_SRC: u64 = 0x5352_43f0;
const DOMAIN_EDGE_OFF: u64 = 0x4f46_46f0;
const DOMAIN_POOL_ID: u64 = 0x4944_f000;
const DOMAIN_PAIR_I: u64 = 0x5041_49f0;
const DOMAIN_PAIR_K: u64 = 0x5041_4bf0;

/// A portable, deterministic 64-bit mixer (SplitMix64). Stable across platforms and toolchains.
pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One reproducible draw keyed by `(seed, domain, index)`.
///
/// Non-commutative in `(domain, index)`: the domain keys an independent stream and the index
/// offsets it, so two different domains can't alias by swapping roles with an index.
fn mix(
    seed: u64,
    domain: u64,
    index: u64,
) -> u64 {
    let keyed = splitmix64(seed ^ domain);
    splitmix64(keyed.wrapping_add(index.wrapping_mul(0x9E37_79B9_7F4A_7C15)))
}

/// The knobs that fully determine a synthetic dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetSpec {
    pub seed: u64,
    pub nodes: usize,
    pub edges: usize,
}

impl DatasetSpec {
    /// Validate the knobs: at least two nodes (so shortest-path endpoints can differ and `id`s fit
    /// `i32`), at most `i32::MAX` nodes (the only integer `QueryParam` width), and at least `nodes`
    /// edges (so the ring backbone that guarantees connectivity fits within the edge budget).
    pub fn validate(&self) -> BenchmarkResult<()> {
        if self.nodes < 2 {
            return Err(OtherError(format!(
                "dataset needs at least 2 nodes (got {})",
                self.nodes
            )));
        }
        if self.nodes > i32::MAX as usize {
            return Err(OtherError(format!(
                "dataset nodes ({}) exceeds the i32 id range",
                self.nodes
            )));
        }
        if self.edges < self.nodes {
            return Err(OtherError(format!(
                "dataset edges ({}) must be >= nodes ({}) so the connected ring backbone fits",
                self.edges, self.nodes
            )));
        }
        if self.edges > i64::MAX as usize {
            return Err(OtherError(format!("dataset edges ({}) too large", self.edges)));
        }
        Ok(())
    }

    /// Deterministic age for node `id` (an un-indexed property the label-scan op filters on).
    fn node_age(
        &self,
        id: i32,
    ) -> i32 {
        18 + (mix(self.seed, DOMAIN_AGE, id as u64) % 60) as i32
    }

    /// The `e`-th directed `:Friend` edge as `(src_id, dst_id)`.
    ///
    /// The first `nodes` edges form a ring `i -> (i mod nodes) + 1` (a connected backbone that
    /// guarantees every node is reachable and gives shortest-path/expansions structure). Any edges
    /// beyond that are seeded-random with a non-zero offset, so `src != dst` without retry loops.
    fn edge_at(
        &self,
        e: usize,
    ) -> (i32, i32) {
        let n = self.nodes as u64;
        if (e as u64) < n {
            let src = e as u64 + 1;
            let dst = (src % n) + 1;
            (src as i32, dst as i32)
        } else {
            let src0 = mix(self.seed, DOMAIN_EDGE_SRC, e as u64) % n;
            let offset = 1 + (mix(self.seed, DOMAIN_EDGE_OFF, e as u64) % (n - 1));
            let dst0 = (src0 + offset) % n;
            ((src0 + 1) as i32, (dst0 + 1) as i32)
        }
    }

    /// A deterministic, sorted sample of up to [`POOL_IDS`] distinct `:User` ids.
    ///
    /// Uses Floyd's algorithm so it always returns exactly `min(nodes, POOL_IDS)` *distinct* ids
    /// (no rejection-sampling under-fill), deterministically from the seed.
    fn node_id_pool(&self) -> Vec<i32> {
        let n = self.nodes;
        let k = POOL_IDS.min(n);
        if n <= POOL_IDS {
            return (1..=n as i32).collect();
        }
        // Floyd's algorithm: pick k distinct values from [0, n) in O(k), then map to 1-based ids.
        let mut chosen = BTreeSet::<u64>::new();
        for (step, j) in ((n - k) as u64..n as u64).enumerate() {
            let t = mix(self.seed, DOMAIN_POOL_ID, step as u64) % (j + 1);
            let pick = if chosen.contains(&t) { j } else { t };
            chosen.insert(pick);
        }
        chosen.into_iter().map(|v| (v + 1) as i32).collect()
    }

    /// A deterministic sample of up to [`POOL_PAIRS`] `(from, to)` pairs that are guaranteed
    /// reachable within `MAX_PAIR_HOPS` directed ring hops (so bounded shortest-path finds a path).
    /// Returns empty for a degenerate (`nodes < 2`) spec so [`Self::handle`] never panics.
    fn connected_pair_pool(&self) -> Vec<(i32, i32)> {
        if self.nodes < 2 {
            return Vec::new();
        }
        let n = self.nodes as u64;
        let max_k = MAX_PAIR_HOPS.min(self.nodes - 1) as u64; // >= 1 since nodes >= 2
        let count = POOL_PAIRS.min(self.nodes);
        (0..count)
            .map(|j| {
                let from = mix(self.seed, DOMAIN_PAIR_I, j as u64) % n; // 0-based
                let k = 1 + (mix(self.seed, DOMAIN_PAIR_K, j as u64) % max_k);
                let to = (from + k) % n;
                ((from + 1) as i32, (to + 1) as i32)
            })
            .collect()
    }

    /// Build the seeded [`DatasetHandle`] pools this spec implies (no server access). Safe for any
    /// spec: a degenerate (`nodes < 2`) spec yields empty pools rather than panicking.
    pub fn handle(&self) -> DatasetHandle {
        DatasetHandle {
            node_ids: self.node_id_pool(),
            connected_pairs: self.connected_pair_pool(),
        }
    }
}

/// A canonical fingerprint of an operation's fully-rendered parameter corpus: a SHA-256 over every
/// query's `CYPHER <params> <body>` string, in order. Because it captures the actual parameter
/// *values* (not just the query body), a change in how the corpus is sampled — e.g. a different RNG
/// — changes the fingerprint, so [`corpus_hash`] can never equate two genuinely different workloads.
pub fn corpus_fingerprint(corpus: &[Query]) -> String {
    let mut h = Sha256::new();
    for q in corpus {
        h.update(q.to_cypher().as_bytes());
        h.update(b"\n");
    }
    format!("{:x}", h.finalize())
}

/// Compute the workload's `corpus_hash`: an algorithm-tagged SHA-256 over everything that defines
/// the measured workload — generator version, dataset knobs, the corpus seed & size, each selected
/// operation (in execution order) paired with a [`corpus_fingerprint`] of its rendered queries, and
/// a digest of the sampled pools. Two runs are only comparable when their `corpus_hash` matches.
pub fn corpus_hash(
    spec: &DatasetSpec,
    corpus_seed: u64,
    corpus_size: usize,
    op_fingerprints: &[(OpName, String)],
    handle: &DatasetHandle,
) -> String {
    let mut h = Sha256::new();
    h.update(GENERATOR_VERSION.as_bytes());
    h.update(format!(
        "\ndataset:seed={},nodes={},edges={}\ncorpus:seed={},size={}\n",
        spec.seed, spec.nodes, spec.edges, corpus_seed, corpus_size
    ));
    for (op, fp) in op_fingerprints {
        h.update(format!("op={}\ncorpus={}\n", op.as_str(), fp));
    }
    // Pool digest guards against a generator change that alters sampled inputs without a version
    // bump.
    for id in &handle.node_ids {
        h.update(id.to_le_bytes());
    }
    for (a, b) in &handle.connected_pairs {
        h.update(a.to_le_bytes());
        h.update(b.to_le_bytes());
    }
    format!("sha256:{:x}", h.finalize())
}

/// Generate the dataset described by `spec` and bulk-load it into `graph`, **replacing** whatever
/// was there (the graph key is dropped first). Creates the `:User(id)` index, loads nodes then
/// edges in `batch_size` `UNWIND` batches, verifies the final counts, and returns the seeded
/// [`DatasetHandle`] the operation corpora draw from. `load_deadline` bounds each batch.
pub async fn generate_and_load(
    graph: &mut AsyncGraph,
    spec: &DatasetSpec,
    batch_size: usize,
    load_deadline: Duration,
    server_timeout_ms: i64,
) -> BenchmarkResult<DatasetHandle> {
    spec.validate()?;
    if batch_size == 0 {
        return Err(OtherError("dataset batch_size must be greater than 0".to_string()));
    }

    // Clean slate: drop the graph key so we don't load on top of stale data. A "graph doesn't
    // exist yet" error is expected and ignored; anything else (auth/network/wrong type) must abort
    // rather than silently loading into a graph we couldn't clear. Bounded by the load deadline.
    match tokio::time::timeout(load_deadline, graph.delete()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            let msg = format!("{:?}", e);
            if !crate::synthetic::is_empty_graph_key(&msg) {
                return Err(OtherError(format!(
                    "failed to drop graph before generating dataset: {}",
                    msg
                )));
            }
        }
        Err(e) => {
            return Err(OtherError(format!(
                "dropping graph before generating dataset timed out: {}",
                e
            )))
        }
    }

    // (Re)create the id index before any data so every insert maintains it and it's operational
    // throughout.
    exec_drain(
        graph,
        "CREATE INDEX FOR (u:User) ON (u.id)",
        server_timeout_ms,
        load_deadline,
    )
    .await
    .map_err(|e| OtherError(format!("failed to create :User(id) index: {:?}", e)))?;

    // Nodes: UNWIND [{id,age},...] AS row CREATE (u:User) SET u = row
    let mut batch = String::new();
    let mut in_batch = 0usize;
    for id in 1..=spec.nodes as i32 {
        if in_batch > 0 {
            batch.push(',');
        }
        // write! appends directly into the batch buffer (no per-row temporary String).
        let _ = write!(batch, "{{id:{},age:{}}}", id, spec.node_age(id));
        in_batch += 1;
        if in_batch == batch_size {
            flush_nodes(graph, &batch, server_timeout_ms, load_deadline).await?;
            batch.clear();
            in_batch = 0;
        }
    }
    if in_batch > 0 {
        flush_nodes(graph, &batch, server_timeout_ms, load_deadline).await?;
        batch.clear();
    }

    // Edges: UNWIND [{src,dst},...] AS row MATCH (n:User{id:row.src}),(m:User{id:row.dst}) CREATE ...
    in_batch = 0;
    for e in 0..spec.edges {
        let (src, dst) = spec.edge_at(e);
        if in_batch > 0 {
            batch.push(',');
        }
        let _ = write!(batch, "{{src:{},dst:{}}}", src, dst);
        in_batch += 1;
        if in_batch == batch_size {
            flush_edges(graph, &batch, server_timeout_ms, load_deadline).await?;
            batch.clear();
            in_batch = 0;
        }
    }
    if in_batch > 0 {
        flush_edges(graph, &batch, server_timeout_ms, load_deadline).await?;
    }

    // Verify the load produced exactly the requested counts before anyone measures against it.
    let node_count = count(graph, "MATCH (n:User) RETURN count(n)", server_timeout_ms, load_deadline).await?;
    if node_count != spec.nodes as i64 {
        return Err(OtherError(format!(
            "dataset load produced {} nodes, expected {}",
            node_count, spec.nodes
        )));
    }
    let edge_count = count(
        graph,
        "MATCH (:User)-[e:Friend]->(:User) RETURN count(e)",
        server_timeout_ms,
        load_deadline,
    )
    .await?;
    if edge_count != spec.edges as i64 {
        return Err(OtherError(format!(
            "dataset load produced {} edges, expected {}",
            edge_count, spec.edges
        )));
    }

    Ok(spec.handle())
}

async fn flush_nodes(
    graph: &mut AsyncGraph,
    maps: &str,
    server_timeout_ms: i64,
    deadline: Duration,
) -> BenchmarkResult<()> {
    let q = format!("UNWIND [{}] AS row CREATE (u:User) SET u = row", maps);
    exec_drain(graph, &q, server_timeout_ms, deadline).await
}

async fn flush_edges(
    graph: &mut AsyncGraph,
    maps: &str,
    server_timeout_ms: i64,
    deadline: Duration,
) -> BenchmarkResult<()> {
    let q = format!(
        "UNWIND [{}] AS row MATCH (n:User {{id: row.src}}), (m:User {{id: row.dst}}) CREATE (n)-[:Friend]->(m)",
        maps
    );
    exec_drain(graph, &q, server_timeout_ms, deadline).await
}

/// Execute a write query and drain its (empty) result set, bounded by `deadline`.
async fn exec_drain(
    graph: &mut AsyncGraph,
    cypher: &str,
    server_timeout_ms: i64,
    deadline: Duration,
) -> BenchmarkResult<()> {
    let fut = async {
        let mut result = graph
            .query(cypher)
            .with_timeout(server_timeout_ms)
            .execute()
            .await
            .map_err(|e| OtherError(format!("load query failed: {:?}", e)))?;
        while let Some(row) = result.data.next().await {
            row.map_err(|e| OtherError(format!("load row error: {:?}", e)))?;
        }
        Ok::<(), crate::error::BenchmarkError>(())
    };
    tokio::time::timeout(deadline, fut)
        .await
        .map_err(|e| OtherError(format!("load query timed out after {:?}: {}", deadline, e)))?
}

/// Run a `RETURN count(...)` scalar query and read the single i64 result.
async fn count(
    graph: &mut AsyncGraph,
    cypher: &str,
    server_timeout_ms: i64,
    deadline: Duration,
) -> BenchmarkResult<i64> {
    let fut = async {
        let mut result = graph
            .ro_query(cypher)
            .with_timeout(server_timeout_ms)
            .execute()
            .await
            .map_err(|e| OtherError(format!("count query failed: {:?}", e)))?;
        match result.data.next().await {
            Some(Ok(row)) => row
                .try_get_at::<i64>(0)
                .map_err(|e| OtherError(format!("count decode error: {:?}", e))),
            Some(Err(e)) => Err(OtherError(format!("count row error: {:?}", e))),
            None => Err(OtherError("count query returned no rows".to_string())),
        }
    };
    tokio::time::timeout(deadline, fut)
        .await
        .map_err(|e| OtherError(format!("count query timed out: {}", e)))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(seed: u64, nodes: usize, edges: usize) -> DatasetSpec {
        DatasetSpec { seed, nodes, edges }
    }

    #[test]
    fn validate_rejects_bad_knobs() {
        assert!(spec(1, 0, 0).validate().is_err());
        assert!(spec(1, 1, 5).validate().is_err()); // < 2 nodes
        assert!(spec(1, 10, 9).validate().is_err()); // edges < nodes
        assert!(spec(1, 10, 10).validate().is_ok());
        assert!(spec(1, 10, 100).validate().is_ok());
        // nodes beyond the i32 id range are rejected.
        assert!(spec(1, i32::MAX as usize + 1, i32::MAX as usize + 1)
            .validate()
            .is_err());
    }

    #[test]
    fn edges_are_deterministic_and_never_self_loops() {
        let s = spec(42, 50, 400);
        for e in 0..s.edges {
            let (a, b) = s.edge_at(e);
            assert_ne!(a, b, "edge {e} is a self-loop");
            assert!((1..=50).contains(&a) && (1..=50).contains(&b));
            // Deterministic: same spec, same edge.
            assert_eq!(s.edge_at(e), spec(42, 50, 400).edge_at(e));
        }
        // The first `nodes` edges are the ring backbone.
        assert_eq!(s.edge_at(0), (1, 2));
        assert_eq!(s.edge_at(49), (50, 1));
    }

    #[test]
    fn different_seed_changes_edges() {
        let a: Vec<_> = (0..400).map(|e| spec(1, 50, 400).edge_at(e)).collect();
        let b: Vec<_> = (0..400).map(|e| spec(2, 50, 400).edge_at(e)).collect();
        assert_ne!(a, b);
    }

    #[test]
    fn handle_pools_are_deterministic_and_valid() {
        let s = spec(7, 10_000, 50_000);
        let h1 = s.handle();
        let h2 = s.handle();
        assert_eq!(h1.node_ids, h2.node_ids);
        assert_eq!(h1.connected_pairs, h2.connected_pairs);
        assert_eq!(h1.node_ids.len(), POOL_IDS);
        // node_ids are distinct, sorted and in range.
        assert!(h1.node_ids.windows(2).all(|w| w[0] < w[1]));
        assert!(h1.node_ids.iter().all(|&id| (1..=10_000).contains(&id)));
        // Every connected pair is distinct and within MAX_PAIR_HOPS ring steps.
        for (a, b) in &h1.connected_pairs {
            assert_ne!(a, b);
            let n = 10_000i64;
            let fwd = (((*b as i64 - *a as i64) % n) + n) % n;
            assert!((1..=MAX_PAIR_HOPS as i64).contains(&fwd), "pair {a}->{b} not within {MAX_PAIR_HOPS} hops");
        }
    }

    #[test]
    fn small_graph_pools_are_all_ids() {
        let s = spec(3, 8, 20);
        let h = s.handle();
        assert_eq!(h.node_ids, (1..=8).collect::<Vec<i32>>());
        assert!(!h.connected_pairs.is_empty());
    }

    #[test]
    fn corpus_hash_is_stable_and_knob_sensitive() {
        let s = spec(42, 1000, 5000);
        let h = s.handle();
        let bodies = vec![
            (OpName::MatchByIndex, "MATCH (n:User {id: $id}) RETURN n.id".to_string()),
            (OpName::ShortestPath, "…".to_string()),
        ];
        let base = corpus_hash(&s, 0, 256, &bodies, &h);
        // Stable: identical inputs ⇒ identical hash, and it's tagged.
        assert!(base.starts_with("sha256:"));
        assert_eq!(base, corpus_hash(&s, 0, 256, &bodies, &h));
        // Sensitive to every knob.
        assert_ne!(base, corpus_hash(&spec(43, 1000, 5000), 0, 256, &bodies, &spec(43, 1000, 5000).handle()));
        assert_ne!(base, corpus_hash(&spec(42, 1001, 5000), 0, 256, &bodies, &spec(42, 1001, 5000).handle()));
        assert_ne!(base, corpus_hash(&spec(42, 1000, 6000), 0, 256, &bodies, &h));
        assert_ne!(base, corpus_hash(&s, 1, 256, &bodies, &h)); // corpus seed
        assert_ne!(base, corpus_hash(&s, 0, 512, &bodies, &h)); // corpus size
        // Sensitive to op set / order and to a changed query body.
        let reordered = vec![bodies[1].clone(), bodies[0].clone()];
        assert_ne!(base, corpus_hash(&s, 0, 256, &reordered, &h));
        let edited = vec![
            (OpName::MatchByIndex, "MATCH (n:User {id: $id}) RETURN n.id, n.age".to_string()),
            bodies[1].clone(),
        ];
        assert_ne!(base, corpus_hash(&s, 0, 256, &edited, &h));
    }

    #[test]
    fn splitmix64_matches_known_vector() {
        // Golden value pins the portable stream so a refactor can't silently shift determinism.
        assert_eq!(splitmix64(0), 0xE220A8397B1DCDAF);
    }

    #[test]
    fn handle_is_panic_free_for_degenerate_specs() {
        // handle() must not panic even for invalid specs (validate() gates the real path, but
        // direct callers shouldn't hit a modulo-by-zero / underflow).
        for nodes in [0usize, 1] {
            let h = DatasetSpec {
                seed: 1,
                nodes,
                edges: 0,
            }
            .handle();
            assert!(h.connected_pairs.is_empty());
        }
    }

    #[test]
    fn corpus_fingerprint_is_deterministic_and_param_sensitive() {
        use crate::query::QueryBuilder;
        let q = |id: i32| {
            QueryBuilder::new()
                .text("MATCH (n:User {id: $id}) RETURN n.id")
                .param("id", id)
                .build()
        };
        let a = vec![q(1), q(2), q(3)];
        let b = vec![q(1), q(2), q(3)];
        let c = vec![q(1), q(2), q(4)]; // one different parameter value
        assert_eq!(corpus_fingerprint(&a), corpus_fingerprint(&b));
        assert_ne!(corpus_fingerprint(&a), corpus_fingerprint(&c));
    }

    #[test]
    fn node_pool_fills_exactly_when_nodes_just_exceed_cap() {
        // The Floyd sampler returns exactly POOL_IDS distinct ids even when nodes barely exceeds it
        // (the old rejection sampler could under-fill here).
        let h = DatasetSpec {
            seed: 3,
            nodes: POOL_IDS + 1,
            edges: POOL_IDS + 1,
        }
        .handle();
        assert_eq!(h.node_ids.len(), POOL_IDS);
        assert!(h.node_ids.windows(2).all(|w| w[0] < w[1])); // distinct + sorted
    }

    #[test]
    fn corpus_hash_golden_value_is_pinned() {
        // A fixed config must always hash to the same value, on any machine/toolchain — this is the
        // cross-process/version stability the comparability gate depends on. If this ever changes,
        // bump GENERATOR_VERSION deliberately (it invalidates prior comparisons).
        let s = DatasetSpec {
            seed: 42,
            nodes: 1000,
            edges: 5000,
        };
        let bodies = vec![(
            OpName::MatchByIndex,
            "MATCH (n:User {id: $id}) RETURN n.id".to_string(),
        )];
        assert_eq!(
            corpus_hash(&s, 0, 256, &bodies, &s.handle()),
            "sha256:daa1d6d9810babea1faf1871e1884b8803e8b83259430ddecfc0a926bddbbb28"
        );
    }
}

