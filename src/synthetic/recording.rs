//! Record-once / replay-identically: serialize a synthetic workload — the dataset **load script**
//! and the per-operation **measured commands** — to a portable on-disk *bundle*, so the exact same
//! graph and command stream can be loaded and run against multiple FalkorDB versions.
//!
//! ## Why
//! Comparing two FalkorDB versions requires the graph and the measured commands to be **identical**.
//! Re-generating the graph and re-deriving the commands on every run (the pre-record flow) relies on
//! that derivation being byte-stable across tool rebuilds — the dataset is (portable `splitmix64`),
//! but the command corpus is drawn with `rand::StdRng`, whose sequence is *not* guaranteed stable
//! across `rand` versions. Recording captures both **once** so a replay never re-derives.
//!
//! ## Bundle layout (`<dir>/`)
//! - `manifest.json` — [`Manifest`]: versions, dataset knobs, graph name, corpus seed, the ops and
//!   their command counts, and the [`Manifest::workload_hash`].
//! - `graph.jsonl` — one [`GraphRecord`] per line: the ordered load statements
//!   ([`crate::synthetic::dataset::load_statements`]) a loader executes (drop + these + verify).
//! - `commands/<op>.jsonl` — one [`CommandRecord`] per line: the fully-rendered measured queries.
//! - `oracle/<op>.jsonl` (format v3+ only) — one [`OracleRecord`] per line: the §6.3 per-command
//!   mutation outcomes captured online at record time ([`attach_oracle`]), re-verified at replay.
//!   A v3 bundle carries the oracle for **exactly** the oracle-eligible write ops it records —
//!   complete command corpus per op, none anywhere else — enforced at [`load`], [`attach_oracle`]
//!   and replay, so oracle coverage can never silently shrink.
//!
//! The [`Manifest::workload_hash`] is a **length-framed** SHA-256 over the header, every graph
//! record, every op's commands (in order), and — under format v3 — every oracle record, so any
//! edit to the graph, the commands *or* the recorded outcomes is detected on [`load`] (the
//! integrity gate), and two runs are only compared when it matches.
//!
//! Recording is **offline** (a pure function of the spec + seed) — no server is contacted. The one
//! deliberate exception is the §6.3 oracle (`record --oracle`,
//! [`oracle::capture`](crate::synthetic::oracle::capture)): write outcomes are state-dependent and
//! cannot be derived offline, so they are captured **online** and folded in afterwards.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::queries_repository::QueryType;
use crate::synthetic::catalog::{spec, RecordedBudget};
use crate::synthetic::dataset::{
    fixture_statements, load_statements, prepared_statements, DatasetSpec, LoadPhase,
    GENERATOR_VERSION,
};
use crate::synthetic::writes::MutationStats;
use crate::synthetic::{OpKey, OpName};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// On-disk bundle format version for **read-only** bundles — the original format, kept
/// byte-identical (including [`Manifest::workload_hash`]) so existing read bundles and their
/// goldens never change (Phase 7 §7.5).
pub const RECORDING_FORMAT_VERSION: u32 = 1;

/// On-disk bundle format version for bundles containing **write** ops (Phase 7 §3.1): the op
/// **kind** is folded into the `workload_hash` (a v1 hash never covered kind — every v1 op is a
/// read), so a bundle's read/write nature cannot be silently flipped. The version is
/// content-determined at record time: any write op ⇒ v2, all-reads ⇒ v1.
pub const RECORDING_FORMAT_VERSION_WRITES: u32 = 2;

/// On-disk bundle format version for **write** bundles carrying a §6.3 **outcome oracle**:
/// per-command [`MutationStats`] captured online against the pristine base at record time
/// (`oracle/<op>.jsonl`) and re-verified per invocation at replay. Oracle records **are** folded
/// into the `workload_hash` (tagged, length-framed parts), so a recorded outcome cannot be edited
/// without detection; v1/v2 bundles and their hashes stay byte-identical.
///
/// **Frozen legacy layout (§6.3-era).** A v3 bundle carries oracles for exactly the
/// [`LEGACY_V3_ORACLE_OPS`] (the seven-op §6.3 eligible set) and **no prepared load phase** —
/// that is what every v3 bundle ever recorded looks like, and it keeps loading and replaying
/// under its own exact-set rule forever. When §6.4 grew the eligible set to nine and added the
/// prepared phase, the format moved to [`RECORDING_FORMAT_VERSION_ORACLE_PREPARED`] rather than
/// changing v3's meaning in place.
pub const RECORDING_FORMAT_VERSION_ORACLE: u32 = 3;

/// On-disk bundle format version for §6.4 **prepared oracle** bundles — what
/// [`attach_oracle`] mints today: the oracle covers exactly the **live** oracle-eligible set
/// (nine ops, [`shapes::oracle_eligible_names`](crate::synthetic::shapes::oracle_eligible_names))
/// and the recorded graph **must** end with the §6.4 prepared load phase (the state the
/// `REMOVE` shape mutates). Same hash rules as v3 (oracle records hash-bound); the version
/// byte differs, so a v3↔v4 rehash flip is caught by the layout gates even before content.
pub const RECORDING_FORMAT_VERSION_ORACLE_PREPARED: u32 = 4;

/// The newest bundle format this build can [`load`].
const RECORDING_FORMAT_VERSION_MAX: u32 = RECORDING_FORMAT_VERSION_ORACLE_PREPARED;

/// The §6.3-era oracle-eligible write ops — **frozen** as recorded history: this is the exact
/// set a format-v3 bundle must carry oracles for. Never derive it from the live shape registry
/// (that is what [`RECORDING_FORMAT_VERSION_ORACLE_PREPARED`] uses); v3's meaning never changes
/// again, or every existing v3 bundle would retroactively become "corrupt".
const LEGACY_V3_ORACLE_OPS: [&str; 7] = [
    "single_vertex_write",
    "single_vertex_update",
    "single_edge_write",
    "merge_user_insert_path",
    "merge_user_upsert_existing",
    "merge_friend_edge_upsert",
    "foreach_loop_mutation",
];

/// The oracle-eligible op names a bundle of `format_version` is required to cover exactly:
/// the frozen [`LEGACY_V3_ORACLE_OPS`] for v3, the live registry for v4+.
pub(crate) fn oracle_required_ops(
    format_version: u32,
) -> std::collections::BTreeSet<&'static str> {
    if format_version >= RECORDING_FORMAT_VERSION_ORACLE_PREPARED {
        crate::synthetic::shapes::oracle_eligible_names()
    } else {
        LEGACY_V3_ORACLE_OPS.iter().copied().collect()
    }
}

/// The dataset knobs a bundle was recorded from (mirrors [`DatasetSpec`], but owned + serde).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetKnobs {
    pub seed: u64,
    pub nodes: usize,
    pub edges: usize,
}

impl DatasetKnobs {
    fn spec(&self) -> DatasetSpec {
        DatasetSpec {
            seed: self.seed,
            nodes: self.nodes,
            edges: self.edges,
        }
    }
}

/// One recorded operation and how many commands it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpEntry {
    pub name: String,
    /// The op's read/write kind, so [`load`] can rebuild its [`OpKey`] — a built-in [`OpName`] or a
    /// string-keyed dynamic op. Defaults to `Read` for bundles written before this field existed
    /// (v1 records reads only). Under format v2+ (write bundles, Phase 7 §3.1) the kind **is**
    /// folded into the [`Manifest::workload_hash`], so a bundle's read/write nature can't be
    /// silently flipped; v1 (read-only) hashes never covered it and stay byte-identical. In both
    /// formats [`load`] also rejects an entry whose declared kind contradicts its built-in name's
    /// catalog kind — the hash is computable by anyone, so a crafted-but-hash-valid manifest must
    /// not reinterpret a built-in op (e.g. declare the write `create_node` as a read).
    #[serde(default = "default_op_kind")]
    pub kind: QueryType,
    /// Whether this op's result is compared across the A/B (record-once / replay-verbatim) gate.
    /// `true` (the default, and the value for every built-in catalog op) means replay computes and
    /// gates a `result_digest`; `false` marks the op **result-N/A** — still recorded and timed, but
    /// its result is *not* gated, for shapes whose result set isn't byte-stable (LIMIT-without-
    /// ORDER, top-k, float scores — design §3.2 / Decision 4). It is **not** folded into the
    /// [`Manifest::workload_hash`] (it's replay-gating policy, not workload content)
    /// and defaults to `true` for bundles written before this field existed.
    #[serde(default = "default_result_gated")]
    pub result_gated: bool,
    /// Per-op runtime budget replay overlays on its global config ([`RecordedBudget`], design
    /// §3.4), so a heavy recorded shape (e.g. a whole-graph algorithm) can dial its own
    /// samples/warmup/concurrency/cache/timeouts down without perturbing the rest of the bundle.
    /// Defaults to full inheritance — the value for every op recorded before this field existed —
    /// and an inherit budget is omitted when serializing. Like [`Self::result_gated`]
    /// it is **not** folded into the [`Manifest::workload_hash`] (replay policy, not workload
    /// content).
    #[serde(default, skip_serializing_if = "RecordedBudget::is_inherit")]
    pub budget: RecordedBudget,
    /// The engine procedure this op requires (e.g. `algo.maxFlow`), or `None` for plain Cypher —
    /// [`crate::synthetic::shapes::ShapeCapability::procedure`] at record time. Replay probes the
    /// engine's `dbms.procedures()` registry before the reference capture (design §3.5) and
    /// **skips** an op whose procedure is absent instead of failing the replay. Like the other
    /// replay-policy fields it is **not** folded into the [`Manifest::workload_hash`] and defaults
    /// to `None` (never probed/skipped) for bundles written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// How many §6.3 oracle records `oracle/<name>.jsonl` holds — per-command [`MutationStats`]
    /// captured online from the pristine base at record time ([`attach_oracle`]) and re-verified
    /// per invocation at replay. `None` (omitted when serializing, so v1/v2 manifests stay
    /// byte-identical) for latency-only ops and for every pre-oracle bundle; `Some` requires
    /// format v3+ and a write op. Unlike the replay-policy fields above, the oracle records
    /// themselves **are** folded into the [`Manifest::workload_hash`] — an expected outcome is
    /// workload content (replay hard-fails on divergence from it), not tuning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<usize>,
    pub count: usize,
}

/// The read/write kind an [`OpEntry`] without an explicit `kind` deserializes to (v1 read bundles).
fn default_op_kind() -> QueryType {
    QueryType::Read
}

/// The result-gating an [`OpEntry`] without an explicit `result_gated` deserializes to: gated
/// (every op recorded before the field existed had its result digest compared).
fn default_result_gated() -> bool {
    true
}

/// Reject an op name that isn't a safe single-path-component slug.
///
/// A recorded op's name becomes a file stem (`commands/<name>.jsonl`), so a string-keyed name
/// containing a path separator or `..` could otherwise escape the bundle directory on record **or**
/// on [`load`] (from a crafted manifest). Names are restricted to `[A-Za-z0-9_-]+`, which every
/// built-in [`OpName`] already satisfies, so this only constrains dynamic string-keyed ops.
fn validate_op_name(name: &str) -> BenchmarkResult<()> {
    if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(())
    } else {
        Err(OtherError(format!(
            "unsafe operation name {name:?}: names must be non-empty and contain only \
             ASCII letters, digits, '_' or '-'"
        )))
    }
}

/// The bundle manifest (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// On-disk bundle format version (see [`RECORDING_FORMAT_VERSION`]).
    pub format_version: u32,
    /// The dataset generator's algorithm version ([`GENERATOR_VERSION`]) at record time.
    pub generator_version: String,
    /// The `benchmark` crate version that wrote the bundle.
    pub tool_version: String,
    /// Dataset knobs the graph was generated from.
    pub dataset: DatasetKnobs,
    /// The graph key the commands target (and a loader loads into by default).
    pub graph: String,
    /// The seed the per-operation command corpora were drawn with.
    pub corpus_seed: u64,
    /// Load batch size the `graph.jsonl` statements were batched at (recorded for transparency).
    pub batch_size: usize,
    /// The recorded operations, in execution order, with their command counts.
    pub ops: Vec<OpEntry>,
    /// Length-framed SHA-256 (`sha256:…`) over the whole workload (header + graph + commands).
    /// Equal iff two bundles describe byte-identical work.
    pub workload_hash: String,
    /// When the bundle was recorded (epoch seconds; excluded from [`Self::workload_hash`]).
    pub created_at_epoch_secs: u64,
}

/// One line of `graph.jsonl`: a load statement and the phase it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRecord {
    pub seq: usize,
    pub phase: String,
    pub cypher: String,
}

/// One line of `commands/<op>.jsonl`: a fully-rendered measured query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRecord {
    pub seq: usize,
    /// The op's read/write kind tag (`"read"`/`"write"`). Informational on disk and **not** part
    /// of the workload hash, so [`load`] validates it against the op's declared kind instead — a
    /// contradicting tag means a hand-edited/corrupt bundle.
    pub kind: String,
    pub cypher: String,
}

/// One line of `oracle/<op>.jsonl`: the [`MutationStats`] the op's command `seq` effected against
/// the **pristine base** at record time (§6.3) — the outcome replay must reproduce, per
/// invocation, from the same restored base. `seq` is validated contiguous-from-0 at [`load`], and
/// the stats are folded into the workload hash, so records can be neither reordered nor edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleRecord {
    pub seq: usize,
    pub stats: MutationStats,
}

/// A loaded bundle held in memory, ready to load into a server and replay.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub manifest: Manifest,
    /// The ordered load statements (`graph.jsonl`).
    pub graph_statements: Vec<(LoadPhase, String)>,
    /// Each recorded op's ordered commands, in the manifest's op order. The [`OpKey`] carries the
    /// op's stable name + kind (a built-in [`OpName`] or a string-keyed dynamic op).
    pub commands: Vec<(OpKey, Vec<String>)>,
    /// Each oracle-bearing op's recorded per-command outcomes (§6.3), keyed by op name — entry
    /// `i` is the [`MutationStats`] the op's command `i` must effect from the pristine base.
    /// Empty for every pre-oracle (v1/v2) bundle.
    pub oracle: BTreeMap<String, Vec<MutationStats>>,
}

/// Length-framed workload hasher. Every part is prefixed with its byte length (u64 LE) before the
/// bytes, so no concatenation of parts can collide with a different split (e.g. `["ab","c"]` and
/// `["a","bc"]` hash differently). Record (streaming) and [`load`] (from memory) feed it in the
/// same order, so they agree iff the content matches. Under format v2+ each op header also feeds
/// the op's read/write **kind** (v1 hashes stay byte-identical: every v1 op is a read).
struct WorkloadHasher(Sha256, u32);

impl WorkloadHasher {
    /// Start a hasher seeded with the bundle header (everything but the graph/command bodies).
    fn new(
        format_version: u32,
        generator_version: &str,
        dataset: &DatasetKnobs,
        graph: &str,
        corpus_seed: u64,
    ) -> Self {
        let mut h = WorkloadHasher(Sha256::new(), format_version);
        h.part(b"synthbench-recording");
        h.part(&format_version.to_le_bytes());
        h.part(generator_version.as_bytes());
        h.part(&dataset.seed.to_le_bytes());
        h.part(&(dataset.nodes as u64).to_le_bytes());
        h.part(&(dataset.edges as u64).to_le_bytes());
        h.part(graph.as_bytes());
        h.part(&corpus_seed.to_le_bytes());
        h
    }

    /// Feed one length-framed part.
    fn part(
        &mut self,
        bytes: &[u8],
    ) {
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
    }

    /// Feed one graph load statement (tagged `G` so it can't alias a command).
    fn graph_record(
        &mut self,
        phase_tag: &str,
        cypher: &str,
    ) {
        self.part(b"G");
        self.part(phase_tag.as_bytes());
        self.part(cypher.as_bytes());
    }

    /// Feed one operation header (name + command count, tagged `O`). Under format v2+ the op's
    /// read/write `kind` follows (tagged `K`) — absent from v1 hashes, whose ops are all reads.
    fn op_header(
        &mut self,
        name: &str,
        count: usize,
        kind: QueryType,
    ) {
        self.part(b"O");
        self.part(name.as_bytes());
        self.part(&(count as u64).to_le_bytes());
        if self.1 >= RECORDING_FORMAT_VERSION_WRITES {
            self.part(b"K");
            self.part(command_kind(kind).as_bytes());
        }
    }

    /// Feed one measured command (tagged `C`).
    fn command(
        &mut self,
        cypher: &str,
    ) {
        self.part(b"C");
        self.part(cypher.as_bytes());
    }

    /// Feed one §6.3 oracle record (tagged `M`, v3+): the record's index followed by the seven
    /// [`MutationStats`] counters, fixed-width little-endian. The on-disk `seq` is validated
    /// contiguous-from-0 **before** the hash recompute, so feeding the canonical index here binds
    /// it — a reordered file fails structurally, an edited counter fails the hash.
    fn oracle_record(
        &mut self,
        seq: usize,
        stats: &MutationStats,
    ) {
        self.part(b"M");
        self.part(&(seq as u64).to_le_bytes());
        let counters = [
            stats.nodes_created,
            stats.nodes_deleted,
            stats.relationships_created,
            stats.relationships_deleted,
            stats.properties_set,
            stats.properties_removed,
            stats.labels_removed,
        ];
        let mut bytes = [0u8; 56];
        for (i, c) in counters.into_iter().enumerate() {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&c.to_le_bytes());
        }
        self.part(&bytes);
    }

    fn finalize(self) -> String {
        format!("sha256:{:x}", self.0.finalize())
    }
}

/// The `kind` string a [`CommandRecord`] (and the v2 hash) carries for the given [`QueryType`].
fn command_kind(kind: QueryType) -> &'static str {
    match kind {
        QueryType::Read => "read",
        QueryType::Write => "write",
    }
}

/// One operation to record, with its already-rendered measured command corpus. Lets callers record
/// ops that have **no catalog `OperationSpec`** — string-keyed `queries_repository` shapes — by
/// supplying the rendered commands directly, while built-in ops are rendered by [`record`] via
/// [`render_commands`]. `key` carries the op's stable name and read/write kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedOp {
    pub key: OpKey,
    /// Whether replay gates this op's result digest — see [`OpEntry::result_gated`]. `true` for
    /// every built-in catalog op and byte-stable shape; `false` marks a result-N/A shape.
    pub result_gated: bool,
    /// Per-op replay budget — see [`OpEntry::budget`]. Inherit (the default) for every current op.
    pub budget: RecordedBudget,
    /// The engine procedure this op requires, if any — see [`OpEntry::capability`].
    pub capability: Option<String>,
    pub commands: Vec<String>,
}

/// De-duplicate recorded ops preserving first-occurrence order (matching how a run executes them),
/// keyed on the stable op name.
fn dedup_recorded(ops: &[RecordedOp]) -> Vec<RecordedOp> {
    let mut seen = std::collections::BTreeSet::new();
    ops.iter()
        .filter(|op| seen.insert(op.key.name().to_string()))
        .cloned()
        .collect()
}

/// Render one operation's measured command corpus exactly as a run/bench derives it: seed
/// `corpus_seed ^ op.salt()`, build the corpus from the spec's handle, render each to its cached
/// cypher. Shared by [`record`] so a recorded command list matches what a generated run would send.
pub fn render_commands(
    op: OpName,
    dataset: &DatasetSpec,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<String>> {
    let handle = dataset.handle();
    let mut rng = StdRng::seed_from_u64(corpus_seed ^ op.salt());
    let corpus = spec(op).build_corpus(&mut rng, &handle, 0, 1)?;
    Ok(corpus.iter().map(|q| q.to_cypher()).collect())
}

/// Record a workload bundle to `out_dir` (created if absent) for the given built-in catalog read
/// `ops`. **Offline** — no server is contacted. Renders each op's corpus via [`render_commands`]
/// then delegates to [`record_rendered`]; **catalog write ops are rejected** — their semantics
/// depend on live scratch state ([`WritePlan`](crate::synthetic::catalog::OperationSpec) hooks),
/// so they cannot be replayed verbatim. Recordable writes are the repo write *shapes*
/// (`--repo-writes`, Phase 7 §1).
pub fn record(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[OpName],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    // Reject catalog writes up-front — before rendering — so we never build a corpus whose
    // semantics depend on the live write worker's scratch/reset hooks.
    if let Some(op) = ops.iter().find(|op| spec(**op).kind == QueryType::Write) {
        return Err(OtherError(format!(
            "catalog write op '{}' cannot be recorded (its scratch/reset hooks don't replay \
             verbatim) — record the repo write shapes with --repo-writes instead",
            op.as_str()
        )));
    }
    let mut recorded = Vec::with_capacity(ops.len());
    for &op in ops {
        recorded.push(RecordedOp {
            key: OpKey::from(op),
            // Every built-in catalog op projects byte-stable scalars, so its result is gated.
            result_gated: true,
            // Propagate the catalog's per-op budget (inherit for every current op) so replay
            // applies the same overrides a generated run applies from the spec.
            budget: spec(op).budget.into(),
            // Catalog ops are plain Cypher — no procedure to probe for.
            capability: None,
            commands: render_commands(op, dataset, corpus_seed)?,
        });
    }
    record_rendered(dataset, graph, &recorded, corpus_seed, batch_size, out_dir)
}

/// Record a workload bundle from **already-rendered** ops — the general form behind [`record`], used
/// for string-keyed shapes that have no catalog `OperationSpec`. **Offline** — no server is
/// contacted.
///
/// Writes `graph.jsonl` (the [`load_statements`] for `dataset`), `commands/<op>.jsonl` for each op,
/// and `manifest.json` with the [`Manifest::workload_hash`]. `ops` are de-duplicated by name (first
/// occurrence wins). Write ops are supported (Phase 7 §3.1) but **not mixed with reads** — replay
/// has one global concurrency sweep, so a mixed bundle cannot express C=1 writes alongside swept
/// reads (design §4); an all-write bundle is stamped format v2 (kind folded into the hash), an
/// all-read bundle stays byte-identical v1. Returns the manifest.
pub fn record_rendered(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[RecordedOp],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    record_rendered_impl(dataset, graph, ops, corpus_seed, batch_size, out_dir, ExtraLoad::None)
}

/// Like [`record_rendered`], but also appends the post-load [`fixture_statements`] (the fulltext +
/// vector index DDL and their deterministic seed data) to `graph.jsonl`, folded into the
/// [`Manifest::workload_hash`]. Used when a recording includes the FixtureDependent read shapes so
/// every replay endpoint gets the identical fulltext/vector fixture (record-once → replay-verbatim).
/// A bundle written this way stays byte-identical across replays; the fixture statements are constant
/// (no `spec`/seed-derived values). The seed `SET`s are inherently idempotent; the index DDL
/// (`CREATE FULLTEXT/VECTOR INDEX …`) assumes a **fresh load** — which replay guarantees by dropping
/// and reloading the graph before executing `graph.jsonl` (design §3.4). The fixture DDL is
/// FalkorDB-specific, so these shapes are for FalkorDB-vs-FalkorDB A/B, not cross-database runs.
pub fn record_rendered_with_fixture(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[RecordedOp],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    record_rendered_impl(dataset, graph, ops, corpus_seed, batch_size, out_dir, ExtraLoad::Fixture)
}

/// Like [`record_rendered`], but also appends the post-load [`prepared_statements`] (the
/// deterministic prepared state the state-dependent write shapes address, design §6.4) to
/// `graph.jsonl`, folded into the [`Manifest::workload_hash`]. Used for `--repo-writes` bundles so
/// `remove_user_property_and_label` removes a property/label that actually exists — and because
/// the oracle's per-invocation `restore_base` replays the full `graph.jsonl`, the prepared state
/// is re-established before **every** captured command (a REMOVE is never a stale no-op). The
/// statement is constant (no `spec`/seed-derived values) and plain Cypher, so write bundles remain
/// engine-agnostic.
pub fn record_rendered_with_prepared(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[RecordedOp],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    record_rendered_impl(dataset, graph, ops, corpus_seed, batch_size, out_dir, ExtraLoad::Prepared)
}

/// Which optional statement block [`record_rendered_impl`] appends to `graph.jsonl` after the base
/// load statements: the FixtureDependent read fixture, the §6.4 prepared write state, or nothing.
/// The two never coexist — a bundle is all-read or all-write.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExtraLoad {
    None,
    Fixture,
    Prepared,
}

impl ExtraLoad {
    /// The extra statements, in load order, as a boxed iterator (`None` contributes nothing).
    fn statements(self) -> Box<dyn Iterator<Item = (LoadPhase, String)>> {
        match self {
            ExtraLoad::None => Box::new(std::iter::empty()),
            ExtraLoad::Fixture => Box::new(fixture_statements()),
            ExtraLoad::Prepared => Box::new(prepared_statements()),
        }
    }
}

/// Shared body of [`record_rendered`] / [`record_rendered_with_fixture`] /
/// [`record_rendered_with_prepared`]. The `extra` statements are streamed into `graph.jsonl` after
/// the base load statements and hashed in the same order, so the entry points differ only by which
/// optional block is present.
fn record_rendered_impl(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[RecordedOp],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
    extra: ExtraLoad,
) -> BenchmarkResult<Manifest> {
    dataset.validate()?;
    if batch_size == 0 {
        return Err(OtherError("record batch_size must be greater than 0".to_string()));
    }
    // Validate every name up-front (a name becomes a file stem), and reject a mixed read+write
    // bundle on the *original* ops (before dedup) so a duplicate name can't hide a kind. Replay
    // has one global concurrency sweep — a mixed bundle cannot express C=1 writes alongside swept
    // reads (Phase 7 §4), so read and write recordings stay separate bundles.
    for op in ops {
        validate_op_name(op.key.name())?;
    }
    let has_writes = ops.iter().any(|op| op.key.kind() == QueryType::Write);
    let has_reads = ops.iter().any(|op| op.key.kind() == QueryType::Read);
    if has_writes && has_reads {
        return Err(OtherError(
            "cannot record a mixed read+write bundle — record writes (--repo-writes) separately \
             from reads (replay measures the two under different policies)"
            .to_string(),
        ));
    }
    // Fail fast on a mismatched extra-load block (Copilot round 3): the fixture is FalkorDB
    // index DDL for the FixtureDependent READ shapes — appending it to a write bundle would break
    // engine-agnostic write recording; the prepared state exists solely for the §6.4 write shapes.
    if extra == ExtraLoad::Fixture && has_writes {
        return Err(OtherError(
            "the fulltext/vector fixture is for read bundles — cannot record a write bundle with \
             fixture statements"
                .to_string(),
        ));
    }
    if extra == ExtraLoad::Prepared && !has_writes {
        return Err(OtherError(
            "the §6.4 prepared state is for write bundles — cannot record a read bundle with \
             prepared statements"
                .to_string(),
        ));
    }
    // The write latency tier asserts nothing (Phase 7 §4.1), so a result-gated write op could be
    // recorded but never replayed (replay hard-rejects it) — fail early here instead.
    if let Some(op) =
        ops.iter().find(|op| op.key.kind() == QueryType::Write && op.result_gated)
    {
        return Err(OtherError(format!(
            "write op '{}' is marked result-gated — the write latency tier asserts nothing \
             (Phase 7 §4.1), so a result-gated write bundle could never be replayed",
            op.key.name()
        )));
    }
    // The write latency tier is algorithm-free plain Cypher (Phase 7 §4.1): no engine procedure to
    // probe for, so a capability on a write op is meaningless — and `capability` is outside the
    // workload hash, so replay independently re-rejects it (a capability-skip would silently
    // shrink the all-ten write coverage).
    if let Some(op) =
        ops.iter().find(|op| op.key.kind() == QueryType::Write && op.capability.is_some())
    {
        return Err(OtherError(format!(
            "write op '{}' declares capability '{}' — write ops are plain Cypher and never \
             capability-gated (Phase 7 §4.1)",
            op.key.name(),
            op.capability.as_deref().unwrap_or_default()
        )));
    }
    // Content-determined format version: any write op ⇒ v2 (kind folded into the workload hash);
    // an all-read bundle stays v1, byte-identical to every bundle recorded before writes existed.
    let format_version = if has_writes {
        RECORDING_FORMAT_VERSION_WRITES
    } else {
        RECORDING_FORMAT_VERSION
    };
    let ops = dedup_recorded(ops);
    if ops.is_empty() {
        return Err(OtherError(
            "no operations to record — pass at least one read --op".to_string(),
        ));
    }

    let knobs = DatasetKnobs {
        seed: dataset.seed,
        nodes: dataset.nodes,
        edges: dataset.edges,
    };
    let commands_dir = out_dir.join("commands");
    std::fs::create_dir_all(&commands_dir)
        .map_err(|e| OtherError(format!("creating {}: {}", commands_dir.display(), e)))?;
    // Recording replaces the bundle wholesale with a v1/v2 manifest that can never reference an
    // oracle — clear any stale oracle/ left by a previous v3 recording (or an interrupted attach)
    // in the same directory, or the fresh bundle would fail load()'s stray-file gate.
    let stale_oracle = out_dir.join("oracle");
    if stale_oracle.is_dir() {
        std::fs::remove_dir_all(&stale_oracle).map_err(|e| {
            OtherError(format!("removing stale {}: {}", stale_oracle.display(), e))
        })?;
    }

    let mut hasher = WorkloadHasher::new(
        format_version,
        GENERATOR_VERSION,
        &knobs,
        graph,
        corpus_seed,
    );

    // graph.jsonl — streamed straight from the lazy statement iterator (one batch in memory). When
    // an extra block (fixture / prepared state) is requested, its statements follow the base load
    // statements in the same stream, so they are written and hashed in that exact order
    // (index → nodes → edges → extra).
    let graph_path = out_dir.join("graph.jsonl");
    {
        let mut w = BufWriter::new(create_file(&graph_path)?);
        let stmts = load_statements(dataset, batch_size).chain(extra.statements());
        for (seq, (phase, stmt)) in stmts.enumerate() {
            hasher.graph_record(phase.tag(), &stmt);
            let rec = GraphRecord {
                seq,
                phase: phase.tag().to_string(),
                cypher: stmt,
            };
            write_jsonl(&mut w, &graph_path, &rec)?;
        }
        w.flush()
            .map_err(|e| OtherError(format!("flushing {}: {}", graph_path.display(), e)))?;
    }

    // commands/<op>.jsonl.
    let mut op_entries = Vec::with_capacity(ops.len());
    for op in &ops {
        let name = op.key.name();
        let cyphers = &op.commands;
        if cyphers.is_empty() {
            return Err(OtherError(format!("operation '{}' produced an empty corpus", name)));
        }
        hasher.op_header(name, cyphers.len(), op.key.kind());
        let path = commands_dir.join(format!("{}.jsonl", name));
        let mut w = BufWriter::new(create_file(&path)?);
        for (seq, cypher) in cyphers.iter().enumerate() {
            hasher.command(cypher);
            let rec = CommandRecord {
                seq,
                kind: command_kind(op.key.kind()).to_string(),
                cypher: cypher.clone(),
            };
            write_jsonl(&mut w, &path, &rec)?;
        }
        w.flush()
            .map_err(|e| OtherError(format!("flushing {}: {}", path.display(), e)))?;
        op_entries.push(OpEntry {
            name: name.to_string(),
            kind: op.key.kind(),
            result_gated: op.result_gated,
            budget: op.budget.clone(),
            capability: op.capability.clone(),
            oracle: None,
            count: cyphers.len(),
        });
    }

    let manifest = Manifest {
        format_version,
        generator_version: GENERATOR_VERSION.to_string(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        dataset: knobs,
        graph: graph.to_string(),
        corpus_seed,
        batch_size,
        ops: op_entries,
        workload_hash: hasher.finalize(),
        created_at_epoch_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let manifest_path = out_dir.join("manifest.json");
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| OtherError(format!("serializing manifest: {}", e)))?;
    std::fs::write(&manifest_path, json)
        .map_err(|e| OtherError(format!("writing {}: {}", manifest_path.display(), e)))?;
    Ok(manifest)
}

/// Load a bundle from `dir`, **verifying its integrity**: the manifest's format version must match,
/// every op's command count must match, and the [`Manifest::workload_hash`] recomputed from the
/// on-disk graph + commands must equal the stored one — so a corrupted or hand-edited bundle is
/// rejected rather than silently replayed.
pub fn load(dir: &Path) -> BenchmarkResult<Bundle> {
    let manifest: Manifest = {
        let path = dir.join("manifest.json");
        let bytes = std::fs::read(&path)
            .map_err(|e| OtherError(format!("reading {}: {}", path.display(), e)))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| OtherError(format!("parsing {}: {}", path.display(), e)))?
    };
    if manifest.format_version < RECORDING_FORMAT_VERSION
        || manifest.format_version > RECORDING_FORMAT_VERSION_MAX
    {
        return Err(OtherError(format!(
            "unsupported recording format_version {} (this build supports {}..={})",
            manifest.format_version, RECORDING_FORMAT_VERSION, RECORDING_FORMAT_VERSION_MAX
        )));
    }
    // A v1 bundle predates write support: every v1 op is a read and v1 hashes never covered kind,
    // so a v1 manifest naming a write op is crafted/corrupt — reject it before the (kind-blind)
    // v1 hash recompute could pass it.
    if manifest.format_version < RECORDING_FORMAT_VERSION_WRITES {
        if let Some(entry) = manifest.ops.iter().find(|e| e.kind == QueryType::Write) {
            return Err(OtherError(format!(
                "format_version {} bundle names write op '{}' — v1 bundles are read-only \
                 (write bundles are format_version {}+)",
                manifest.format_version, entry.name, RECORDING_FORMAT_VERSION_WRITES
            )));
        }
    }
    // The oracle (§6.3) is a v3 feature: a pre-v3 manifest naming an oracle count was hand-edited
    // (its hash never covered oracle records, so the kind-blind recompute below could pass it),
    // and a v3 bundle with NO oracle should have been recorded as v2 — reject both, so the format
    // version states exactly what the bundle carries.
    let oracle_entries = manifest.ops.iter().filter(|e| e.oracle.is_some()).count();
    if manifest.format_version < RECORDING_FORMAT_VERSION_ORACLE && oracle_entries > 0 {
        return Err(OtherError(format!(
            "format_version {} bundle declares an outcome oracle — oracle bundles are \
             format_version {}+ (crafted/corrupt manifest)",
            manifest.format_version, RECORDING_FORMAT_VERSION_ORACLE
        )));
    }
    if manifest.format_version >= RECORDING_FORMAT_VERSION_ORACLE && oracle_entries == 0 {
        return Err(OtherError(format!(
            "format_version {} bundle declares no outcome oracle for any op — the oracle is \
             what distinguishes v{} from v{} (crafted/corrupt manifest)",
            manifest.format_version,
            RECORDING_FORMAT_VERSION_ORACLE,
            RECORDING_FORMAT_VERSION_WRITES
        )));
    }
    // Exact-set enforcement (§6.3): an oracle bundle must carry an oracle for EVERY recorded
    // write op its format version deems eligible ([`oracle_required_ops`]: the frozen legacy
    // seven for v3, the live nine-op registry for v4+), covering its COMPLETE command corpus,
    // and for nothing else — so oracle coverage can never silently shrink (a crafted proper
    // subset, or a padded oracle on an op outside the eligible set, is rejected here rather
    // than replayed).
    if manifest.format_version >= RECORDING_FORMAT_VERSION_ORACLE {
        let eligible = oracle_required_ops(manifest.format_version);
        for entry in &manifest.ops {
            let is_eligible =
                entry.kind == QueryType::Write && eligible.contains(entry.name.as_str());
            match (is_eligible, entry.oracle) {
                (true, None) => {
                    return Err(OtherError(format!(
                        "oracle-eligible op '{}' carries no outcome oracle — a v{} bundle must \
                         cover every eligible op (§6.3 exact-set rule; crafted/corrupt manifest)",
                        entry.name, manifest.format_version
                    )));
                }
                (true, Some(n)) if n != entry.count => {
                    return Err(OtherError(format!(
                        "op '{}' declares {} oracle record(s) for {} command(s) — the §6.3 \
                         oracle covers the complete corpus (crafted/corrupt manifest)",
                        entry.name, n, entry.count
                    )));
                }
                (false, Some(_)) if entry.kind != QueryType::Write => {
                    return Err(OtherError(format!(
                        "manifest declares an outcome oracle for read op '{}' — the oracle \
                         records mutation counters, which only write ops produce \
                         (crafted/corrupt manifest)",
                        entry.name
                    )));
                }
                (false, Some(_)) => {
                    return Err(OtherError(format!(
                        "manifest declares an outcome oracle for op '{}', which is not \
                         oracle-eligible under format v{} (crafted/corrupt manifest)",
                        entry.name, manifest.format_version
                    )));
                }
                _ => {}
            }
        }
    }

    // graph.jsonl → ordered (phase, cypher).
    let graph_path = dir.join("graph.jsonl");
    let graph_records: Vec<GraphRecord> = read_jsonl(&graph_path)?;
    let mut graph_statements = Vec::with_capacity(graph_records.len());
    for rec in &graph_records {
        let phase = LoadPhase::from_tag(&rec.phase).ok_or_else(|| {
            OtherError(format!(
                "{}: unknown load phase '{}'",
                graph_path.display(),
                rec.phase
            ))
        })?;
        graph_statements.push((phase, rec.cypher.clone()));
    }
    // Format ↔ layout gates on the (hash-bound) prepared phase: v3 is the frozen §6.3-era
    // layout that PREDATES the prepared state, v4 is defined by it — so a version flip on a
    // rehashed bundle is caught structurally, not just by the eligible-set difference.
    let has_prepared = graph_statements.iter().any(|(p, _)| *p == LoadPhase::Prepared);
    let has_write_ops = manifest.ops.iter().any(|e| e.kind == QueryType::Write);
    if has_prepared && !has_write_ops {
        return Err(OtherError(
            "bundle carries a §6.4 prepared load phase but records no write ops — the prepared \
             state exists solely for the write shapes (crafted/corrupt bundle)"
                .to_string(),
        ));
    }
    if manifest.format_version == RECORDING_FORMAT_VERSION_ORACLE && has_prepared {
        return Err(OtherError(format!(
            "format_version {} bundle carries a §6.4 prepared load phase — v{} is the frozen \
             §6.3-era layout that predates the prepared state; prepared oracle bundles are \
             format_version {} (crafted/downgraded manifest)",
            manifest.format_version,
            RECORDING_FORMAT_VERSION_ORACLE,
            RECORDING_FORMAT_VERSION_ORACLE_PREPARED
        )));
    }
    if manifest.format_version >= RECORDING_FORMAT_VERSION_ORACLE_PREPARED && !has_prepared {
        return Err(OtherError(format!(
            "format_version {} bundle lacks the §6.4 prepared load phase — v{} bundles record \
             the prepared state the REMOVE shape mutates (crafted/upgraded manifest)",
            manifest.format_version, RECORDING_FORMAT_VERSION_ORACLE_PREPARED
        )));
    }

    // commands/<op>.jsonl for each op named in the manifest, in order.
    let mut commands = Vec::with_capacity(manifest.ops.len());
    let mut seen_names = std::collections::BTreeSet::new();
    for entry in &manifest.ops {
        // Reject an unsafe name before it becomes a file path — a crafted manifest name with a path
        // separator or `..` must not read outside the bundle's `commands/` directory.
        validate_op_name(&entry.name)?;
        // Reject duplicate op names: op names key `commands/<name>.jsonl` and the replay report's
        // per-op map, so a duplicate would double-run or silently overwrite a result. A recorded
        // bundle is deduped at record time, so a duplicate here means a crafted/corrupt manifest.
        if !seen_names.insert(entry.name.as_str()) {
            return Err(OtherError(format!(
                "manifest lists duplicate op name '{}'",
                entry.name
            )));
        }
        // Rebuild the op identity from its name + kind. `OpKey::dynamic` canonicalizes a built-in
        // name back to its `OpName` (keeping the built-in salt/kind); a name with no `OpName`
        // becomes a string-keyed dynamic op. Either way the bundle round-trips by name.
        let op = OpKey::dynamic(entry.name.clone(), entry.kind);
        // …but that canonicalization IGNORES the manifest kind for a built-in name, so a crafted
        // manifest declaring a built-in under the wrong kind (e.g. the write op `create_node` as a
        // `read`) would be silently reinterpreted with the catalog kind — sidestepping the v1
        // read-only gate above and, under v1's kind-blind hash, the integrity check too. Reject
        // the mismatch instead of reinterpreting it.
        if op.kind() != entry.kind {
            return Err(OtherError(format!(
                "manifest declares op '{}' as kind '{}', but that name is the built-in '{}' op — \
                 a bundle cannot reinterpret a built-in op's kind (crafted/corrupt manifest)",
                entry.name,
                command_kind(entry.kind),
                command_kind(op.kind()),
            )));
        }
        let path = dir.join("commands").join(format!("{}.jsonl", entry.name));
        let recs: Vec<CommandRecord> = read_jsonl(&path)?;
        if recs.len() != entry.count {
            return Err(OtherError(format!(
                "{}: has {} commands but manifest says {}",
                path.display(),
                recs.len(),
                entry.count
            )));
        }
        // The per-command `kind` tag is informational and unhashed, so a hand-edited tag would
        // survive the workload-hash gate below — validate it against the op's declared kind here
        // to keep the bundle self-consistent.
        if let Some(rec) = recs.iter().find(|r| r.kind != command_kind(entry.kind)) {
            return Err(OtherError(format!(
                "{}: command seq {} declares kind '{}' but op '{}' is a {} op — the bundle is \
                 corrupted or was edited",
                path.display(),
                rec.seq,
                rec.kind,
                entry.name,
                command_kind(entry.kind)
            )));
        }
        commands.push((op, recs.into_iter().map(|r| r.cypher).collect::<Vec<_>>()));
    }

    // oracle/<op>.jsonl for each oracle-bearing op (§6.3, v3+): the manifest-level shape was
    // validated by the exact-set pass above; here validate the files structurally (record count,
    // contiguous seq) BEFORE the hash recompute below, so a malformed oracle fails with an
    // actionable error naming the op instead of a bare hash mismatch. The stats themselves are
    // hash-bound — see [`WorkloadHasher::oracle_record`].
    let mut oracle: BTreeMap<String, Vec<MutationStats>> = BTreeMap::new();
    for entry in &manifest.ops {
        let Some(count) = entry.oracle else { continue };
        let path = dir.join("oracle").join(format!("{}.jsonl", entry.name));
        let recs: Vec<OracleRecord> = read_jsonl(&path)?;
        if recs.len() != count {
            return Err(OtherError(format!(
                "{}: has {} oracle record(s) but manifest says {}",
                path.display(),
                recs.len(),
                count
            )));
        }
        if let Some((i, rec)) = recs.iter().enumerate().find(|(i, rec)| rec.seq != *i) {
            return Err(OtherError(format!(
                "{}: oracle record {} declares seq {} — records must be contiguous from 0 \
                 (the bundle is corrupted or was edited)",
                path.display(),
                i,
                rec.seq
            )));
        }
        oracle.insert(entry.name.clone(), recs.into_iter().map(|r| r.stats).collect());
    }
    // Reject stray oracle files not named by the manifest: they would be dead, UNHASHED content
    // in a bundle that claims integrity (and a signal of a hand-edited bundle).
    let oracle_dir = dir.join("oracle");
    if oracle_dir.is_dir() {
        let listing = std::fs::read_dir(&oracle_dir)
            .map_err(|e| OtherError(format!("reading {}: {}", oracle_dir.display(), e)))?;
        for dirent in listing {
            let dirent =
                dirent.map_err(|e| OtherError(format!("reading {}: {}", oracle_dir.display(), e)))?;
            let file_name = dirent.file_name();
            let file_name = file_name.to_string_lossy();
            let known = file_name
                .strip_suffix(".jsonl")
                .is_some_and(|stem| oracle.contains_key(stem));
            if !known {
                let hint = if manifest.format_version < RECORDING_FORMAT_VERSION_ORACLE {
                    " — likely an interrupted `record --oracle` attach; delete the bundle's \
                     oracle/ directory or re-run the oracle capture to repair"
                } else {
                    ""
                };
                return Err(OtherError(format!(
                    "{}: unexpected oracle file '{}' — not declared by any manifest op \
                     (the bundle is corrupted or was edited){}",
                    oracle_dir.display(),
                    file_name,
                    hint
                )));
            }
        }
    }

    // Recompute the workload hash from the on-disk content and gate on it.
    let mut hasher = WorkloadHasher::new(
        manifest.format_version,
        &manifest.generator_version,
        &manifest.dataset,
        &manifest.graph,
        manifest.corpus_seed,
    );
    for (phase, cypher) in &graph_statements {
        hasher.graph_record(phase.tag(), cypher);
    }
    for ((_, cyphers), entry) in commands.iter().zip(&manifest.ops) {
        hasher.op_header(&entry.name, cyphers.len(), entry.kind);
        for cypher in cyphers {
            hasher.command(cypher);
        }
        // §6.3: the op's oracle records follow its commands in the hash stream (v3+; validated
        // contiguous above, so the canonical index binds each record's position).
        if let Some(stats) = oracle.get(&entry.name) {
            for (seq, s) in stats.iter().enumerate() {
                hasher.oracle_record(seq, s);
            }
        }
    }
    let recomputed = hasher.finalize();
    if recomputed != manifest.workload_hash {
        return Err(OtherError(format!(
            "recording integrity check failed for {}: workload_hash mismatch \
             (manifest {}, recomputed {}) — the bundle is corrupted or was edited",
            dir.display(),
            manifest.workload_hash,
            recomputed
        )));
    }

    Ok(Bundle {
        manifest,
        graph_statements,
        commands,
        oracle,
    })
}

impl Bundle {
    /// The dataset spec the bundle was recorded from.
    pub fn spec(&self) -> DatasetSpec {
        self.manifest.dataset.spec()
    }
}

/// Fold captured §6.3/§6.4 oracle outcomes into an existing **v2 write bundle**, upgrading it in
/// place to format **v4** ([`RECORDING_FORMAT_VERSION_ORACLE_PREPARED`]): write
/// `oracle/<op>.jsonl` for every op in `oracle`, mark each op's manifest entry
/// with its record count, and recompute the [`Manifest::workload_hash`] under oracle rules (the
/// oracle records are hash-bound — see [`WorkloadHasher::oracle_record`]). The bundle must carry
/// the §6.4 prepared load phase (every write bundle recorded by this build does); the frozen
/// legacy v3 layout is load/replay-only and can never be minted again. The upgraded bundle is
/// re-[`load`]ed as the final step, so the function returns exactly what every future load will
/// verify — any inconsistency this function could write fails here, at record time.
///
/// The capture itself (running commands against a live engine) lives in
/// [`oracle::capture`](crate::synthetic::oracle::capture); this function is the offline half.
/// Remove an orphaned `oracle/` directory left by an **interrupted attach**: a pre-v3 manifest
/// cannot reference oracle entries, so an `oracle/` directory sitting next to one is provably
/// dead content that would otherwise brick every retry on [`load`]'s stray-file gate. A v3+
/// manifest (or an unreadable one) is left untouched — [`load`] stays the arbiter there.
pub(crate) fn heal_orphaned_oracle(dir: &Path) -> BenchmarkResult<()> {
    let manifest_path = dir.join("manifest.json");
    if let Ok(bytes) = std::fs::read(&manifest_path) {
        if let Ok(peek) = serde_json::from_slice::<Manifest>(&bytes) {
            let orphaned = dir.join("oracle");
            if peek.format_version < RECORDING_FORMAT_VERSION_ORACLE && orphaned.is_dir() {
                std::fs::remove_dir_all(&orphaned).map_err(|e| {
                    OtherError(format!(
                        "removing orphaned {} (interrupted previous attach): {}",
                        orphaned.display(),
                        e
                    ))
                })?;
            }
        }
    }
    Ok(())
}

pub fn attach_oracle(
    dir: &Path,
    oracle: &BTreeMap<String, Vec<MutationStats>>,
) -> BenchmarkResult<Manifest> {
    heal_orphaned_oracle(dir)?;
    // Verify the bundle as it stands before touching it (hash gate included).
    let bundle = load(dir)?;
    if bundle.manifest.format_version >= RECORDING_FORMAT_VERSION_ORACLE {
        return Err(OtherError(format!(
            "{} already carries an outcome oracle (format_version {}) — re-record instead of \
             re-attaching",
            dir.display(),
            bundle.manifest.format_version
        )));
    }
    if oracle.is_empty() {
        return Err(OtherError("no oracle outcomes to attach (empty capture)".to_string()));
    }
    // Exact-set enforcement (§6.3): the oracle must cover every recorded oracle-eligible write op
    // with its COMPLETE command corpus — no subset, no strays — so v4 coverage can never silently
    // shrink below the tier the format version promises.
    let eligible = oracle_required_ops(RECORDING_FORMAT_VERSION_ORACLE_PREPARED);
    for entry in &bundle.manifest.ops {
        let is_eligible =
            entry.kind == QueryType::Write && eligible.contains(entry.name.as_str());
        match (is_eligible, oracle.get(&entry.name)) {
            (true, None) => {
                return Err(OtherError(format!(
                    "no oracle captured for oracle-eligible op '{}' — a v{} bundle must cover \
                     every eligible op (§6.3 exact-set rule)",
                    entry.name, RECORDING_FORMAT_VERSION_ORACLE_PREPARED
                )));
            }
            (true, Some(outcomes)) if outcomes.len() != entry.count => {
                return Err(OtherError(format!(
                    "oracle for op '{}' has {} outcome(s) but the op records {} command(s) — \
                     the §6.3 oracle covers the complete corpus",
                    entry.name,
                    outcomes.len(),
                    entry.count
                )));
            }
            (false, Some(_)) => {
                return Err(OtherError(format!(
                    "oracle captured for op '{}', which is not oracle-eligible (the live \
                     eligible set only)",
                    entry.name
                )));
            }
            _ => {}
        }
    }
    if let Some(name) =
        oracle.keys().find(|name| !bundle.manifest.ops.iter().any(|e| &e.name == *name))
    {
        return Err(OtherError(format!(
            "oracle captured for op '{}', which the bundle does not record",
            name
        )));
    }
    // v4 is defined by the §6.4 prepared phase: every write bundle this build records carries
    // it, so its absence means a stale/crafted bundle that must be re-recorded, not upgraded.
    if !bundle.graph_statements.iter().any(|(p, _)| *p == LoadPhase::Prepared) {
        return Err(OtherError(format!(
            "{} lacks the §6.4 prepared load phase — a v{} oracle bundle records the prepared \
             state; re-record the bundle with this build instead of attaching to a stale one",
            dir.display(),
            RECORDING_FORMAT_VERSION_ORACLE_PREPARED
        )));
    }

    // oracle/<op>.jsonl.
    let oracle_dir = dir.join("oracle");
    std::fs::create_dir_all(&oracle_dir)
        .map_err(|e| OtherError(format!("creating {}: {}", oracle_dir.display(), e)))?;
    for (name, outcomes) in oracle {
        let path = oracle_dir.join(format!("{}.jsonl", name));
        let mut w = BufWriter::new(create_file(&path)?);
        for (seq, stats) in outcomes.iter().enumerate() {
            write_jsonl(&mut w, &path, &OracleRecord { seq, stats: *stats })?;
        }
        w.flush().map_err(|e| OtherError(format!("flushing {}: {}", path.display(), e)))?;
    }

    // Upgraded manifest: v4, per-op oracle counts, hash recomputed over the full oracle stream.
    let mut manifest = bundle.manifest.clone();
    manifest.format_version = RECORDING_FORMAT_VERSION_ORACLE_PREPARED;
    for entry in &mut manifest.ops {
        entry.oracle = oracle.get(&entry.name).map(Vec::len);
    }
    let mut hasher = WorkloadHasher::new(
        manifest.format_version,
        &manifest.generator_version,
        &manifest.dataset,
        &manifest.graph,
        manifest.corpus_seed,
    );
    for (phase, cypher) in &bundle.graph_statements {
        hasher.graph_record(phase.tag(), cypher);
    }
    for ((_, cyphers), entry) in bundle.commands.iter().zip(&manifest.ops) {
        hasher.op_header(&entry.name, cyphers.len(), entry.kind);
        for cypher in cyphers {
            hasher.command(cypher);
        }
        if let Some(stats) = oracle.get(&entry.name) {
            for (seq, s) in stats.iter().enumerate() {
                hasher.oracle_record(seq, s);
            }
        }
    }
    manifest.workload_hash = hasher.finalize();

    let manifest_path = dir.join("manifest.json");
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| OtherError(format!("serializing manifest: {}", e)))?;
    std::fs::write(&manifest_path, json)
        .map_err(|e| OtherError(format!("writing {}: {}", manifest_path.display(), e)))?;

    // Prove the upgraded bundle round-trips through the same gate every replay will use.
    let reloaded = load(dir)?;
    Ok(reloaded.manifest)
}

fn create_file(path: &Path) -> BenchmarkResult<std::fs::File> {
    std::fs::File::create(path).map_err(|e| OtherError(format!("creating {}: {}", path.display(), e)))
}

fn write_jsonl<T: Serialize, W: Write>(
    w: &mut W,
    path: &Path,
    value: &T,
) -> BenchmarkResult<()> {
    let line =
        serde_json::to_string(value).map_err(|e| OtherError(format!("serializing a record: {}", e)))?;
    writeln!(w, "{}", line).map_err(|e| OtherError(format!("writing {}: {}", path.display(), e)))
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> BenchmarkResult<Vec<T>> {
    let file =
        std::fs::File::open(path).map_err(|e| OtherError(format!("reading {}: {}", path.display(), e)))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| OtherError(format!("reading {}: {}", path.display(), e)))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: T = serde_json::from_str(&line).map_err(|e| {
            OtherError(format!("{}: bad JSON on line {}: {}", path.display(), i + 1, e))
        })?;
        out.push(value);
    }
    Ok(out)
}

/// A convenience for tests/tools: a unique temp directory path (not created). Unique even across
/// concurrent callers in one process (a process-wide counter), so parallel tests can't collide.
pub fn temp_bundle_dir(prefix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{}-{}-{}-{}", prefix, std::process::id(), nanos, seq))
}

#[cfg(test)]
pub(crate) mod test_forge {
    //! Test-only bundle forgery: build hash-valid oracle bundles in arbitrary layouts —
    //! including the frozen legacy v3 layout and the crafted cross-version flips load() must
    //! reject — bypassing `attach_oracle`'s gates. Shared by the recording and replay tests.
    use super::*;

    /// Recompute the on-disk bundle's `workload_hash` from its raw files under the manifest's
    /// declared `format_version` and rewrite `manifest.json` — the same stream `load()` hashes,
    /// read without `load()`'s gates so a forged layout can be made hash-valid.
    pub(crate) fn rehash_bundle(dir: &Path) {
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let graph_records: Vec<GraphRecord> = read_jsonl(&dir.join("graph.jsonl")).unwrap();
        let mut hasher = WorkloadHasher::new(
            manifest.format_version,
            &manifest.generator_version,
            &manifest.dataset,
            &manifest.graph,
            manifest.corpus_seed,
        );
        for rec in &graph_records {
            hasher.graph_record(&rec.phase, &rec.cypher);
        }
        for entry in &manifest.ops {
            let recs: Vec<CommandRecord> =
                read_jsonl(&dir.join("commands").join(format!("{}.jsonl", entry.name))).unwrap();
            hasher.op_header(&entry.name, recs.len(), entry.kind);
            for rec in &recs {
                hasher.command(&rec.cypher);
            }
            if entry.oracle.is_some() {
                let orecs: Vec<OracleRecord> =
                    read_jsonl(&dir.join("oracle").join(format!("{}.jsonl", entry.name)))
                        .unwrap();
                for (seq, r) in orecs.iter().enumerate() {
                    hasher.oracle_record(seq, &r.stats);
                }
            }
        }
        manifest.workload_hash = hasher.finalize();
        std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap())
            .unwrap();
    }

    /// Forge a hash-valid oracle bundle at `dir`: one `CREATE` command per `op_names` entry
    /// (each oracled with its true outcome, `nodes_created = 1`, so a live replay verifies),
    /// with or without the §6.4 prepared load phase, stamped with an arbitrary
    /// `format_version`. Layout validity is deliberately NOT checked — that is load()'s job.
    pub(crate) fn forge_oracle_bundle(
        dir: &Path,
        op_names: &[&str],
        prepared: bool,
        format_version: u32,
    ) {
        let spec = DatasetSpec {
            seed: 3,
            nodes: 50,
            edges: 100,
        };
        let ops: Vec<RecordedOp> = op_names
            .iter()
            .map(|name| RecordedOp {
                key: OpKey::dynamic(*name, QueryType::Write),
                result_gated: false,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CREATE (:ForgedOracleProbe)".to_string()],
            })
            .collect();
        if prepared {
            record_rendered_with_prepared(&spec, "g", &ops, 3, 32, dir).unwrap();
        } else {
            record_rendered(&spec, "g", &ops, 3, 32, dir).unwrap();
        }
        let oracle_dir = dir.join("oracle");
        std::fs::create_dir_all(&oracle_dir).unwrap();
        let stats = MutationStats {
            nodes_created: 1,
            ..MutationStats::default()
        };
        for name in op_names {
            let path = oracle_dir.join(format!("{}.jsonl", name));
            let mut w = BufWriter::new(create_file(&path).unwrap());
            write_jsonl(&mut w, &path, &OracleRecord { seq: 0, stats }).unwrap();
            w.flush().unwrap();
        }
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.format_version = format_version;
        for entry in &mut manifest.ops {
            entry.oracle = Some(1);
        }
        std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap())
            .unwrap();
        rehash_bundle(dir);
    }

    /// The frozen legacy §6.3 op set, exposed for tests that forge v3 bundles.
    pub(crate) fn legacy_v3_ops() -> Vec<&'static str> {
        LEGACY_V3_ORACLE_OPS.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_ops() -> Vec<OpName> {
        vec![OpName::MatchByIndex, OpName::Expand1Hop, OpName::AggregateCount]
    }

    fn record_to_temp(seed: u64) -> (PathBuf, Manifest) {
        let dir = temp_bundle_dir("synthrec-test");
        let spec = DatasetSpec {
            seed,
            nodes: 200,
            edges: 600,
        };
        let manifest = record(&spec, "gtest", &read_ops(), seed, 64, &dir).unwrap();
        (dir, manifest)
    }

    #[test]
    fn record_then_load_round_trips() {
        let (dir, manifest) = record_to_temp(7);
        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.manifest, manifest);
        assert_eq!(bundle.manifest.graph, "gtest");
        assert_eq!(bundle.manifest.ops.len(), 3);
        // graph statements equal the generator's statements for the same spec/batch.
        let spec = bundle.spec();
        let want: Vec<(LoadPhase, String)> = load_statements(&spec, 64).collect();
        assert_eq!(bundle.graph_statements, want);
        // commands equal what a run would derive for each op.
        for (op, cyphers) in &bundle.commands {
            let name = OpName::from_tag(op.name()).expect("built-in op name");
            assert_eq!(*cyphers, render_commands(name, &spec, manifest.corpus_seed).unwrap());
            assert!(!cyphers.is_empty());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recording_is_deterministic_across_two_records() {
        // The sanity check's core: the same config records to an identical workload_hash.
        let (dir_a, man_a) = record_to_temp(42);
        let (dir_b, man_b) = record_to_temp(42);
        assert_eq!(man_a.workload_hash, man_b.workload_hash);
        // A different seed changes the hash (different data + commands).
        let (dir_c, man_c) = record_to_temp(43);
        assert_ne!(man_a.workload_hash, man_c.workload_hash);
        for d in [dir_a, dir_b, dir_c] {
            std::fs::remove_dir_all(&d).ok();
        }
    }

    #[test]
    fn load_rejects_a_tampered_command() {
        let (dir, _man) = record_to_temp(1);
        // Flip a byte in one command line — counts still match, but the hash won't.
        let path = dir.join("commands").join("match_by_index.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let tampered = text.replacen("RETURN", "return", 1);
        assert_ne!(text, tampered, "expected a RETURN to rewrite");
        std::fs::write(&path, tampered).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(
            format!("{}", err).contains("integrity check failed"),
            "unexpected error: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_tampered_graph_statement() {
        let (dir, _man) = record_to_temp(2);
        let path = dir.join("graph.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        // Change an age value in the first node batch.
        let tampered = text.replacen("age:", "age:1", 1);
        std::fs::write(&path, tampered).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{}", err).contains("integrity check failed"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_command_count_mismatch() {
        let (dir, _man) = record_to_temp(3);
        // Drop the last command line from one op file → count no longer matches the manifest.
        let path = dir.join("commands").join("expand_1_hop.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.pop();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{}", err).contains("manifest says"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rejects_write_ops() {
        let dir = temp_bundle_dir("synthrec-write");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let err = record(&spec, "g", &[OpName::CreateNode], 1, 8, &dir).unwrap_err();
        assert!(format!("{}", err).contains("catalog write op"), "got: {err}");
        assert!(format!("{}", err).contains("--repo-writes"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_dedups_ops_and_rejects_empty() {
        let dir = temp_bundle_dir("synthrec-dedup");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let manifest = record(
            &spec,
            "g",
            &[OpName::MatchByIndex, OpName::MatchByIndex],
            1,
            8,
            &dir,
        )
        .unwrap();
        assert_eq!(manifest.ops.len(), 1);
        std::fs::remove_dir_all(&dir).ok();

        let dir2 = temp_bundle_dir("synthrec-empty");
        assert!(record(&spec, "g", &[], 1, 8, &dir2).is_err());
    }

    #[test]
    fn op_entry_kind_defaults_to_read_when_absent() {
        // A v1 bundle (written before `kind` existed) recorded reads only — a kind-less entry must
        // deserialize to `Read` via `default_op_kind`, and an explicit kind round-trips.
        let legacy: OpEntry = serde_json::from_str(r#"{"name":"match_by_index","count":3}"#).unwrap();
        assert_eq!(legacy.kind, QueryType::Read);
        assert_eq!(legacy.count, 3);
        let explicit: OpEntry =
            serde_json::from_str(r#"{"name":"w","kind":"Write","count":1}"#).unwrap();
        assert_eq!(explicit.kind, QueryType::Write);
    }

    #[test]
    fn record_rendered_round_trips_a_dynamic_op() {
        // A string-keyed op with no built-in `OpName`, recorded from hand-supplied commands (the
        // path a `queries_repository` shape will use), survives `load` with the integrity gate.
        let dir = temp_bundle_dir("synthrec-dyn");
        let spec = DatasetSpec {
            seed: 3,
            nodes: 50,
            edges: 150,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("single_vertex_read", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec![
                "CYPHER id=1 MATCH (n:User {id:$id}) RETURN n".to_string(),
                "CYPHER id=2 MATCH (n:User {id:$id}) RETURN n".to_string(),
            ],
        }];
        let manifest = record_rendered(&spec, "gdyn", &ops, 9, 32, &dir).unwrap();
        assert_eq!(manifest.ops.len(), 1);
        assert_eq!(manifest.ops[0].name, "single_vertex_read");
        assert_eq!(manifest.ops[0].kind, QueryType::Read);
        assert_eq!(manifest.ops[0].count, 2);

        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.manifest, manifest);
        assert_eq!(bundle.commands.len(), 1);
        let (key, cmds) = &bundle.commands[0];
        assert_eq!(key.name(), "single_vertex_read");
        assert!(!key.is_named(), "an unknown name loads back as a dynamic op");
        assert_eq!(key.kind(), QueryType::Read);
        assert_eq!(cmds, &ops[0].commands);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_with_fixture_appends_fixture_and_changes_hash() {
        // Recording the FixtureDependent shapes bakes the fulltext/vector fixture into `graph.jsonl`
        // (record-once → replay-verbatim) and folds it into the workload_hash.
        let dir = temp_bundle_dir("synthrec-fixture");
        let spec = DatasetSpec {
            seed: 5,
            nodes: 200,
            edges: 400,
        };
        let ops = vec![RecordedOp {
            // Mirrors a fixture-dependent shape: a non-gated (result-N/A) read.
            key: OpKey::dynamic("vector_query_nodes_smoke", QueryType::Read),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec![
                "CALL db.idx.vector.queryNodes('User', 'embedding', 10, vecf32([0.1, 0.2, 0.3])) \
                 YIELD node, score RETURN id(node), score LIMIT 10"
                    .to_string(),
            ],
        }];
        let with = record_rendered_with_fixture(&spec, "gfix", &ops, 9, 32, &dir).unwrap();

        // The bundle survives the integrity gate and its graph is base load stmts + the fixture.
        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.manifest, with);
        let base: Vec<(LoadPhase, String)> = load_statements(&spec, 32).collect();
        let fixture: Vec<(LoadPhase, String)> = fixture_statements().collect();
        let want: Vec<(LoadPhase, String)> = base.iter().chain(fixture.iter()).cloned().collect();
        assert_eq!(bundle.graph_statements, want);
        // The trailing statements are exactly the fixture phase, in order.
        let tail = &bundle.graph_statements[bundle.graph_statements.len() - fixture.len()..];
        assert_eq!(tail, fixture.as_slice());
        // The recorded op stays non-gated (result-N/A) through the round-trip.
        assert!(!bundle.manifest.ops[0].result_gated);

        // Recording the same spec/ops *without* the fixture yields a different workload_hash, proving
        // the fixture is folded into the hash (so it can't be silently dropped on replay).
        let dir2 = temp_bundle_dir("synthrec-nofixture");
        let without = record_rendered(&spec, "gfix", &ops, 9, 32, &dir2).unwrap();
        assert_ne!(with.workload_hash, without.workload_hash);

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn record_rendered_with_prepared_appends_prepared_state_and_changes_hash() {
        // A --repo-writes recording bakes the §6.4 prepared-state statement into `graph.jsonl`
        // (so REMOVE targets state that exists, re-established by every oracle restore) and folds
        // it into the workload_hash.
        let dir = temp_bundle_dir("synthrec-prepared");
        let spec = DatasetSpec {
            seed: 5,
            nodes: 200,
            edges: 400,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("remove_user_property_and_label", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec![
                "MATCH (u:User {id: 3}) REMOVE u.rpc_social_credit, u:TemporaryLabel RETURN u.id"
                    .to_string(),
            ],
        }];
        let with = record_rendered_with_prepared(&spec, "gprep", &ops, 9, 32, &dir).unwrap();

        // The bundle survives the integrity gate and its graph is base load stmts + prepared.
        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.manifest, with);
        let base: Vec<(LoadPhase, String)> = load_statements(&spec, 32).collect();
        let prepared: Vec<(LoadPhase, String)> = prepared_statements().collect();
        let want: Vec<(LoadPhase, String)> = base.iter().chain(prepared.iter()).cloned().collect();
        assert_eq!(bundle.graph_statements, want);
        // The trailing statements are exactly the prepared phase, in order.
        let tail = &bundle.graph_statements[bundle.graph_statements.len() - prepared.len()..];
        assert_eq!(tail, prepared.as_slice());
        // A write-only bundle keeps the kind-bound format (v2 — no oracle data attached here).
        assert_eq!(bundle.manifest.format_version, RECORDING_FORMAT_VERSION_WRITES);

        // Recording the same spec/ops *without* the prepared state yields a different
        // workload_hash, proving it is folded into the hash (it can't be silently dropped).
        let dir2 = temp_bundle_dir("synthrec-noprepared");
        let without = record_rendered(&spec, "gprep", &ops, 9, 32, &dir2).unwrap();
        assert_ne!(with.workload_hash, without.workload_hash);

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn record_rejects_a_mismatched_extra_load_block() {
        // Fail-fast guards: fixture on a write bundle (FalkorDB DDL would break engine-agnostic
        // write recording) and prepared state on a read bundle are both constructor misuse.
        let spec = DatasetSpec {
            seed: 5,
            nodes: 200,
            edges: 400,
        };
        let write_op = RecordedOp {
            key: OpKey::dynamic("w_solo", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["MATCH (u:User {id: 1}) SET u.x = 1".to_string()],
        };
        let read_op = RecordedOp {
            key: OpKey::dynamic("r_solo", QueryType::Read),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["MATCH (u:User {id: 1}) RETURN u.id".to_string()],
        };

        let dir = temp_bundle_dir("synthrec-fixture-on-write");
        let err = record_rendered_with_fixture(&spec, "g", &[write_op], 9, 32, &dir).unwrap_err();
        assert!(
            format!("{err}").contains("fixture is for read bundles"),
            "got: {err}"
        );

        let dir2 = temp_bundle_dir("synthrec-prepared-on-read");
        let err = record_rendered_with_prepared(&spec, "g", &[read_op], 9, 32, &dir2).unwrap_err();
        assert!(
            format!("{err}").contains("prepared state is for write bundles"),
            "got: {err}"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn load_canonicalizes_builtin_names_to_named_ops() {
        // Built-in read ops recorded via `record` load back as `Named` keys (canonicalized by name),
        // so the built-in salt/kind is preserved across a record→load round-trip.
        let (dir, _man) = record_to_temp(11);
        let bundle = load(&dir).unwrap();
        assert!(bundle.commands.iter().all(|(k, _)| k.is_named()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_writes_a_format_v2_write_bundle_that_round_trips() {
        // A write-only bundle is stamped format v2 (Phase 7 §3.1): the op kind lands in the
        // manifest, in every CommandRecord, and in the workload hash; `load` verifies it intact.
        let dir = temp_bundle_dir("synthrec-dynwrite");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("bulk_insert", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CYPHER x=1 CREATE (n:User {id:$x})".to_string()],
        }];
        let manifest = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        assert_eq!(manifest.format_version, RECORDING_FORMAT_VERSION_WRITES);
        assert_eq!(manifest.ops[0].kind, QueryType::Write);
        // The on-disk command record carries the write kind.
        let raw = std::fs::read_to_string(dir.join("commands").join("bulk_insert.jsonl")).unwrap();
        let rec: CommandRecord = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(rec.kind, "write");
        // And the bundle loads back with the kind preserved + the integrity gate green.
        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.commands.len(), 1);
        assert_eq!(bundle.commands[0].0.kind(), QueryType::Write);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_bundle_workload_hash_binds_the_op_kind() {
        // Flipping a v2 bundle's op kind must fail the hash recompute on `load` — the kind is
        // hashed (v2), so a bundle's read/write nature can't be silently flipped even by a
        // self-consistent edit that also flips every unhashed per-command kind tag.
        let dir = temp_bundle_dir("synthrec-kindflip");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("w", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CYPHER  CREATE (n)".to_string()],
        }];
        record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        let path = dir.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        manifest["ops"][0]["kind"] = serde_json::json!("Read");
        std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        let cmd_path = dir.join("commands").join("w.jsonl");
        let retagged: String = std::fs::read_to_string(&cmd_path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut rec: CommandRecord = serde_json::from_str(line).unwrap();
                rec.kind = "read".to_string();
                serde_json::to_string(&rec).unwrap() + "\n"
            })
            .collect();
        std::fs::write(&cmd_path, retagged).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{err}").contains("workload_hash mismatch"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_command_record_with_a_contradicting_kind() {
        // The per-command `kind` tag is informational and unhashed, so flipping it on disk keeps
        // the workload hash valid — `load` must still reject the contradiction against the op's
        // declared kind (and via the kind gate, not the hash gate).
        let (dir, manifest) = record_to_temp(19);
        let name = manifest.ops[0].name.clone();
        let path = dir.join("commands").join(format!("{name}.jsonl"));
        let flipped: String = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut rec: CommandRecord = serde_json::from_str(line).unwrap();
                rec.kind = "write".to_string();
                serde_json::to_string(&rec).unwrap() + "\n"
            })
            .collect();
        std::fs::write(&path, flipped).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("declares kind 'write'"), "got: {msg}");
        assert!(msg.contains(&format!("op '{name}' is a read op")), "got: {msg}");
        assert!(
            !msg.contains("workload_hash"),
            "the unhashed tag must be caught by the kind gate, not the hash gate: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_v1_bundle_naming_a_write_op() {
        // A v1 manifest naming a write op is crafted/corrupt (v1 predates writes, and its hash
        // never covered kind) — rejected explicitly, before the kind-blind v1 hash recompute.
        let dir = temp_bundle_dir("synthrec-v1write");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("w", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CYPHER  CREATE (n)".to_string()],
        }];
        record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        let path = dir.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        manifest["format_version"] = serde_json::json!(1);
        std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{err}").contains("v1 bundles are read-only"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_bundles_stay_format_v1() {
        // The format version is content-determined: an all-read bundle must keep writing v1 so
        // every pre-Phase-7 read bundle, hash and golden stays byte-identical (§7.5).
        let (dir, manifest) = record_to_temp(17);
        assert_eq!(manifest.format_version, RECORDING_FORMAT_VERSION);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_rejects_an_empty_corpus() {
        let dir = temp_bundle_dir("synthrec-dynempty");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("empty_shape", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec![],
        }];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        assert!(format!("{}", err).contains("empty corpus"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_dedups_by_name_keeping_first() {
        let dir = temp_bundle_dir("synthrec-dyndedup");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![
            RecordedOp {
                key: OpKey::dynamic("a_read", QueryType::Read),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("a_read", QueryType::Read),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  RETURN 2".to_string()],
            },
        ];
        let manifest = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        assert_eq!(manifest.ops.len(), 1);
        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.commands[0].1, vec!["CYPHER  RETURN 1".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_op_name_accepts_slugs_and_rejects_traversal() {
        for ok in ["match_by_index", "expand_1hop", "shape-42", "A_b-9"] {
            assert!(validate_op_name(ok).is_ok(), "{ok} should be accepted");
        }
        for bad in ["", "../evil", "a/b", "a\\b", "a.b", "..", "with space", "emoji_🚀"] {
            assert!(validate_op_name(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn record_rendered_rejects_unsafe_names() {
        let dir = temp_bundle_dir("synthrec-unsafe");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("../escape", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CYPHER  RETURN 1".to_string()],
        }];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        assert!(format!("{err}").contains("unsafe operation name"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_rejects_a_mixed_read_write_bundle() {
        // The kind check runs on the original ops (before dedup), so a same-named write behind a
        // read is caught rather than dropped by first-occurrence dedup. Mixed bundles are rejected
        // outright: replay measures reads and writes under different policies (Phase 7 §4).
        let dir = temp_bundle_dir("synthrec-dupwrite");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![
            RecordedOp {
                key: OpKey::dynamic("dup", QueryType::Read),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("dup", QueryType::Write),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  CREATE (n)".to_string()],
            },
        ];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        assert!(format!("{err}").contains("mixed read+write bundle"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_rejects_a_capability_on_a_write_op() {
        // Phase 7 §4.1: the write latency tier is algorithm-free plain Cypher — a capability on a
        // write op is meaningless at record time (and unhashed, so replay re-rejects it too).
        let dir = temp_bundle_dir("synthrec-writecap");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("w_cap", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: Some("algo.maxFlow".to_string()),
            commands: vec!["CREATE (n)".to_string()],
        }];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        assert!(format!("{err}").contains("never capability-gated"), "got: {err}");
        assert!(format!("{err}").contains("algo.maxFlow"), "must name the capability: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_rejects_a_result_gated_write_op() {
        // Phase 7 §4.1: the write latency tier asserts nothing, and replay hard-rejects a
        // result-gated write — so recording one would produce a bundle that can never be
        // replayed. Fail early, naming the op.
        let dir = temp_bundle_dir("synthrec-writegated");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("w_gated", QueryType::Write),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CREATE (n)".to_string()],
        }];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("could never be replayed"), "got: {msg}");
        assert!(msg.contains("w_gated"), "must name the op: {msg}");
        assert!(!dir.join("manifest.json").exists(), "nothing may be written");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Rewrite the bundle's single op to `new_name`/`new_kind` (renaming its commands file to
    /// match) and **recompute a valid `workload_hash`** over the tampered content — modeling an
    /// attacker who can rewrite the whole bundle (the hash is computable by anyone; it is an
    /// integrity check, not a MAC) but cannot change what a built-in op name *means*.
    fn tamper_op_identity_with_valid_hash(
        dir: &Path,
        new_name: &str,
        new_kind: QueryType,
    ) {
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        let old_name = manifest.ops[0].name.clone();
        manifest.ops[0].name = new_name.to_string();
        manifest.ops[0].kind = new_kind;
        if old_name != new_name {
            std::fs::rename(
                dir.join("commands").join(format!("{old_name}.jsonl")),
                dir.join("commands").join(format!("{new_name}.jsonl")),
            )
            .unwrap();
        }
        let graph_records: Vec<GraphRecord> = read_jsonl(&dir.join("graph.jsonl")).unwrap();
        let mut hasher = WorkloadHasher::new(
            manifest.format_version,
            &manifest.generator_version,
            &manifest.dataset,
            &manifest.graph,
            manifest.corpus_seed,
        );
        for rec in &graph_records {
            hasher.graph_record(&rec.phase, &rec.cypher);
        }
        for entry in &manifest.ops {
            let recs: Vec<CommandRecord> =
                read_jsonl(&dir.join("commands").join(format!("{}.jsonl", entry.name))).unwrap();
            hasher.op_header(&entry.name, recs.len(), entry.kind);
            for rec in &recs {
                hasher.command(&rec.cypher);
            }
        }
        manifest.workload_hash = hasher.finalize();
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
    }

    #[test]
    fn load_rejects_a_v1_bundle_reinterpreting_a_builtin_write_as_read() {
        // The reinterpretation attack, v1 flavor: every v1 entry claims kind read (passing the
        // v1 read-only gate) and the v1 hash is kind-blind — but `OpKey::dynamic` canonicalizes
        // 'create_node' to the built-in WRITE op, ignoring the manifest kind. Before the kind
        // guard, this hash-valid bundle silently became a write workload.
        let dir = temp_bundle_dir("synthrec-kindv1");
        let spec = DatasetSpec {
            seed: 3,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("plain_read", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["RETURN 1".to_string()],
        }];
        let manifest = record_rendered(&spec, "g", &ops, 3, 8, &dir).unwrap();
        assert_eq!(manifest.format_version, RECORDING_FORMAT_VERSION, "reads record as v1");

        tamper_op_identity_with_valid_hash(&dir, "create_node", QueryType::Read);
        let err = load(&dir).unwrap_err();
        assert!(
            format!("{err}").contains("cannot reinterpret a built-in op's kind"),
            "got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_v2_bundle_reinterpreting_a_builtin_kind() {
        // The v2 flavor: v2 hashes the kind, but the hash is computable by anyone — a crafted
        // bundle can declare the built-in write 'create_node' as a read with a perfectly valid
        // recomputed hash. The kind guard, not the hash gate, must reject it.
        let dir = temp_bundle_dir("synthrec-kindv2");
        let spec = DatasetSpec {
            seed: 3,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("w_op", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CREATE (n:X)".to_string()],
        }];
        let manifest = record_rendered(&spec, "g", &ops, 3, 8, &dir).unwrap();
        assert_eq!(manifest.format_version, RECORDING_FORMAT_VERSION_WRITES, "writes are v2");

        // Sanity: renaming to the built-in with the CORRECT kind loads fine — proving the
        // tamper helper produces hash-valid bundles, so the rejection below is the kind guard
        // alone, not the hash gate.
        tamper_op_identity_with_valid_hash(&dir, "create_node", QueryType::Write);
        load(&dir).expect("correct-kind rename with a recomputed hash must load");

        // The attack: same built-in name, kind flipped to read (hash recomputed over 'read').
        tamper_op_identity_with_valid_hash(&dir, "create_node", QueryType::Read);
        let err = load(&dir).unwrap_err();
        assert!(
            format!("{err}").contains("cannot reinterpret a built-in op's kind"),
            "got: {err}"
        );

        // The reverse direction is equally rejected: a built-in READ name declared as a write.
        tamper_op_identity_with_valid_hash(&dir, "match_by_index", QueryType::Write);
        let err = load(&dir).unwrap_err();
        assert!(
            format!("{err}").contains("cannot reinterpret a built-in op's kind"),
            "got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_unsafe_manifest_names() {
        // A crafted manifest whose op name contains a path separator must be rejected on load,
        // before the name is turned into a `commands/<name>.jsonl` path.
        let dir = temp_bundle_dir("synthrec-loadunsafe");
        let spec = DatasetSpec {
            seed: 2,
            nodes: 20,
            edges: 60,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("safe_read", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CYPHER  RETURN 1".to_string()],
        }];
        record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        let manifest_path = dir.join("manifest.json");
        let doctored = std::fs::read_to_string(&manifest_path)
            .unwrap()
            .replace("safe_read", "../evil");
        std::fs::write(&manifest_path, doctored).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{err}").contains("unsafe operation name"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_duplicate_manifest_op_names() {
        // A recorded bundle is deduped at record time; a manifest with two entries sharing a name
        // is crafted/corrupt and must be rejected so replay can't double-run or overwrite by name.
        let dir = temp_bundle_dir("synthrec-dupload");
        let spec = DatasetSpec {
            seed: 2,
            nodes: 20,
            edges: 60,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("safe_read", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CYPHER  RETURN 1".to_string()],
        }];
        record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        let manifest_path = dir.join("manifest.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        let dup = v["ops"][0].clone();
        v["ops"].as_array_mut().unwrap().push(dup);
        std::fs::write(&manifest_path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{err}").contains("duplicate op name"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_persists_result_gated_and_load_round_trips_it() {
        // A bundle can mix a gated op and a result-N/A op; `load` round-trips each op's
        // `result_gated` so replay knows which results to gate (design §3.2 / Decision 4).
        let dir = temp_bundle_dir("synthrec-gated");
        let spec = DatasetSpec {
            seed: 4,
            nodes: 20,
            edges: 60,
        };
        let ops = vec![
            RecordedOp {
                key: OpKey::dynamic("gated_read", QueryType::Read),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("na_read", QueryType::Read),
                result_gated: false,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  MATCH (n) RETURN n LIMIT 1".to_string()],
            },
        ];
        let manifest = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        assert!(manifest.ops[0].result_gated, "first op stays gated");
        assert!(!manifest.ops[1].result_gated, "second op is result-N/A");

        let bundle = load(&dir).unwrap();
        assert!(bundle.manifest.ops[0].result_gated);
        assert!(!bundle.manifest.ops[1].result_gated);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn result_gated_is_not_folded_into_the_workload_hash() {
        // `result_gated` is replay-gating policy, not workload content: two bundles that differ
        // ONLY in it must share a `workload_hash` (so it never perturbs the A/B comparability gate).
        let spec = DatasetSpec {
            seed: 5,
            nodes: 20,
            edges: 60,
        };
        let make = |gated: bool| {
            let dir = temp_bundle_dir(if gated { "synthrec-hg" } else { "synthrec-hn" });
            let ops = vec![RecordedOp {
                key: OpKey::dynamic("shape", QueryType::Read),
                result_gated: gated,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  RETURN 1".to_string()],
            }];
            let m = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
            std::fs::remove_dir_all(&dir).ok();
            m.workload_hash
        };
        assert_eq!(make(true), make(false));
    }

    #[test]
    fn op_entry_defaults_result_gated_true_for_pre_field_bundles() {
        // An `OpEntry` serialized before `result_gated` existed (no such key) deserializes to
        // gated — preserving the pre-field behaviour where every op's result was compared.
        let entry: OpEntry =
            serde_json::from_str(r#"{"name":"legacy_op","count":3}"#).unwrap();
        assert_eq!(entry.kind, QueryType::Read);
        assert!(entry.result_gated, "a pre-field op defaults to gated");
        assert!(entry.budget.is_inherit(), "a pre-field op inherits every global knob");
    }

    #[test]
    fn budget_is_not_folded_into_the_workload_hash() {
        // Like `kind`/`result_gated`, the per-op budget is replay policy, not workload content: two
        // bundles that differ ONLY in a budget must share a `workload_hash`, so budgeting an op
        // never breaks A/B comparability with an unbudgeted recording of the same workload.
        let spec = DatasetSpec {
            seed: 5,
            nodes: 20,
            edges: 60,
        };
        let make = |budget: RecordedBudget| {
            let dir = temp_bundle_dir(if budget.is_inherit() { "synthrec-bi" } else { "synthrec-bb" });
            let ops = vec![RecordedOp {
                key: OpKey::dynamic("shape", QueryType::Read),
                result_gated: true,
                budget,
                capability: None,
                commands: vec!["CYPHER  RETURN 1".to_string()],
            }];
            let m = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
            std::fs::remove_dir_all(&dir).ok();
            m.workload_hash
        };
        let budgeted = RecordedBudget {
            samples: Some(1),
            concurrency: Some(vec![1]),
            ..RecordedBudget::default()
        };
        assert_eq!(make(RecordedBudget::default()), make(budgeted));
    }

    #[test]
    fn budget_round_trips_through_the_manifest_and_inherit_is_omitted() {
        // A budgeted op's overrides survive record → load; an inherit budget is omitted from the
        // manifest JSON entirely, so every pre-field bundle (and the docs' sample manifest) stays
        // byte-compatible.
        let dir = temp_bundle_dir("synthrec-budget");
        let spec = DatasetSpec {
            seed: 5,
            nodes: 20,
            edges: 60,
        };
        let budget = RecordedBudget {
            samples: Some(2),
            warmup: Some(1),
            concurrency: Some(vec![1, 2]),
            cache: Some(crate::synthetic::CacheSelection::Cached),
            server_timeout_ms: Some(30_000),
            client_deadline_ms: Some(31_000),
        };
        let ops = vec![
            RecordedOp {
                key: OpKey::dynamic("heavy_shape", QueryType::Read),
                result_gated: false,
                budget: budget.clone(),
                capability: None,
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("plain_shape", QueryType::Read),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  RETURN 2".to_string()],
            },
        ];
        record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();

        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.manifest.ops[0].budget, budget);
        assert!(bundle.manifest.ops[1].budget.is_inherit());
        // The manifest text carries a `budget` key only for the budgeted op.
        let manifest_json = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert_eq!(manifest_json.matches("\"budget\"").count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capability_is_not_folded_into_the_workload_hash() {
        // Like `kind`/`result_gated`/`budget`, the capability annotation is replay policy (which
        // procedure to probe for), not workload content: two bundles that differ ONLY in a
        // capability must share a `workload_hash`, so annotating an op never breaks A/B
        // comparability with an annotation-free recording of the same workload.
        let spec = DatasetSpec {
            seed: 5,
            nodes: 20,
            edges: 60,
        };
        let make = |capability: Option<String>| {
            let dir = temp_bundle_dir(if capability.is_some() {
                "synthrec-cs"
            } else {
                "synthrec-cn"
            });
            let ops = vec![RecordedOp {
                key: OpKey::dynamic("shape", QueryType::Read),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability,
                commands: vec!["CYPHER  RETURN 1".to_string()],
            }];
            let m = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
            std::fs::remove_dir_all(&dir).ok();
            m.workload_hash
        };
        assert_eq!(make(None), make(Some("algo.maxFlow".to_string())));
    }

    #[test]
    fn capability_round_trips_through_the_manifest_and_none_is_omitted() {
        // An annotated op's required procedure survives record → load; an annotation-free op is
        // omitted from the manifest JSON entirely, so every pre-field bundle stays byte-compatible
        // (and deserializes to `None`).
        let dir = temp_bundle_dir("synthrec-capability");
        let spec = DatasetSpec {
            seed: 5,
            nodes: 20,
            edges: 60,
        };
        let ops = vec![
            RecordedOp {
                key: OpKey::dynamic("algo_shape", QueryType::Read),
                result_gated: false,
                budget: RecordedBudget::default(),
                capability: Some("algo.maxFlow".to_string()),
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("plain_shape", QueryType::Read),
                result_gated: true,
                budget: RecordedBudget::default(),
                capability: None,
                commands: vec!["CYPHER  RETURN 2".to_string()],
            },
        ];
        record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();

        let bundle = load(&dir).unwrap();
        assert_eq!(
            bundle.manifest.ops[0].capability.as_deref(),
            Some("algo.maxFlow")
        );
        assert_eq!(bundle.manifest.ops[1].capability, None);
        // The manifest text carries a `capability` key only for the annotated op.
        let manifest_json = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert_eq!(manifest_json.matches("\"capability\"").count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workload_hash_is_length_framed() {
        // ["ab","c"] and ["a","bc"] must not collide — the length prefix disambiguates.
        let mut h1 = WorkloadHasher(Sha256::new(), RECORDING_FORMAT_VERSION);
        h1.part(b"ab");
        h1.part(b"c");
        let mut h2 = WorkloadHasher(Sha256::new(), RECORDING_FORMAT_VERSION);
        h2.part(b"a");
        h2.part(b"bc");
        assert_ne!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn op_header_hashes_the_kind_only_under_v2() {
        // v1 op headers are kind-blind (byte-compat with every pre-Phase-7 bundle); v2 headers
        // fold the kind in, so read- and write-kinded ops hash differently under v2 only.
        let hash = |version: u32, kind: QueryType| {
            let mut h = WorkloadHasher(Sha256::new(), version);
            h.op_header("op", 1, kind);
            h.finalize()
        };
        assert_eq!(
            hash(RECORDING_FORMAT_VERSION, QueryType::Read),
            hash(RECORDING_FORMAT_VERSION, QueryType::Write)
        );
        assert_ne!(
            hash(RECORDING_FORMAT_VERSION_WRITES, QueryType::Read),
            hash(RECORDING_FORMAT_VERSION_WRITES, QueryType::Write)
        );
    }

    #[test]
    fn load_missing_dir_errors() {
        let err = load(&temp_bundle_dir("synthrec-missing")).unwrap_err();
        assert!(format!("{err}").contains("manifest.json"), "got: {err}");
    }

    #[test]
    fn load_rejects_bad_format_version() {
        let (dir, _man) = record_to_temp(5);
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path).unwrap();
        // Bump the on-disk format version past what this build supports.
        let bad = text.replacen("\"format_version\": 1", "\"format_version\": 9999", 1);
        assert_ne!(text, bad, "expected the format_version to rewrite");
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{err}").contains("format_version"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- §6.3/§6.4 outcome oracle (format v4; frozen legacy v3) ----

    /// A recorded two-op write bundle (2 + 3 commands, §6.4 prepared phase included) for oracle
    /// tests. The ops carry REAL oracle-eligible shape names (`OpKey::dynamic` resolves
    /// built-ins by name) because the exact-set rule pins oracles to exactly the eligible set —
    /// custom rendered names could never be attached.
    fn record_write_bundle_to_temp(prefix: &str) -> PathBuf {
        let dir = temp_bundle_dir(prefix);
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let op = |name: &str, n: usize| RecordedOp {
            key: OpKey::dynamic(name, QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: (0..n)
                .map(|i| format!("CYPHER x={} CREATE (n:User {{id:$x}})", i))
                .collect(),
        };
        record_rendered_with_prepared(
            &spec,
            "g",
            &[op("single_vertex_write", 2), op("single_vertex_update", 3)],
            1,
            8,
            &dir,
        )
        .unwrap();
        dir
    }

    fn stats(nodes_created: i64) -> MutationStats {
        MutationStats {
            nodes_created,
            properties_set: 1,
            ..MutationStats::default()
        }
    }

    /// The exact-set oracle for [`record_write_bundle_to_temp`]: every eligible op, full corpus.
    fn full_oracle() -> BTreeMap<String, Vec<MutationStats>> {
        let mut oracle = BTreeMap::new();
        oracle.insert("single_vertex_write".to_string(), vec![stats(1), stats(2)]);
        oracle.insert("single_vertex_update".to_string(), vec![stats(3), stats(4), stats(5)]);
        oracle
    }

    #[test]
    fn attach_oracle_upgrades_a_v2_bundle_to_v4_and_round_trips() {
        let dir = record_write_bundle_to_temp("synthrec-oracle-rt");
        let v2_hash = load(&dir).unwrap().manifest.workload_hash.clone();
        let manifest = attach_oracle(&dir, &full_oracle()).unwrap();
        assert_eq!(manifest.format_version, RECORDING_FORMAT_VERSION_ORACLE_PREPARED);
        assert_ne!(manifest.workload_hash, v2_hash, "the oracle must land in the hash");
        let write = manifest.ops.iter().find(|e| e.name == "single_vertex_write").unwrap();
        let update = manifest.ops.iter().find(|e| e.name == "single_vertex_update").unwrap();
        assert_eq!((write.oracle, update.oracle), (Some(2), Some(3)));
        // The upgraded bundle loads through the standard gate with the outcomes intact.
        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.oracle["single_vertex_write"], vec![stats(1), stats(2)]);
        assert_eq!(bundle.oracle["single_vertex_update"], vec![stats(3), stats(4), stats(5)]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn attach_oracle_is_deterministic_for_identical_outcomes() {
        // Two identically-recorded bundles with identical captured outcomes must agree on the v3
        // workload_hash — the record-once/attach-once flow stays reproducible end to end.
        let hash_of = |prefix: &str| {
            let dir = record_write_bundle_to_temp(prefix);
            let h = attach_oracle(&dir, &full_oracle()).unwrap().workload_hash;
            std::fs::remove_dir_all(&dir).ok();
            h
        };
        assert_eq!(hash_of("synthrec-oracle-da"), hash_of("synthrec-oracle-db"));
    }

    #[test]
    fn workload_hash_binds_the_oracle_outcomes() {
        // Editing one counter in one oracle record must fail the hash gate: an expected outcome
        // is workload content — replay hard-fails on divergence from it — so it must be
        // tamper-evident like the commands themselves.
        let dir = record_write_bundle_to_temp("synthrec-oracle-tamper");
        attach_oracle(&dir, &full_oracle()).unwrap();
        let path = dir.join("oracle").join("single_vertex_write.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let bad = text.replacen("\"nodes_created\":1", "\"nodes_created\":2", 1);
        assert_ne!(text, bad, "expected the counter to rewrite");
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(format!("{err}").contains("workload_hash mismatch"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_non_contiguous_oracle_seq() {
        // A reordered/renumbered oracle file fails STRUCTURALLY (before the hash recompute), with
        // an error naming the record — more actionable than a bare hash mismatch.
        let dir = record_write_bundle_to_temp("synthrec-oracle-seq");
        attach_oracle(&dir, &full_oracle()).unwrap();
        let path = dir.join("oracle").join("single_vertex_write.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let bad = text.replacen("\"seq\":1", "\"seq\":5", 1);
        assert_ne!(text, bad);
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("contiguous") && msg.contains("seq 5"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_an_oracle_count_mismatch() {
        let dir = record_write_bundle_to_temp("synthrec-oracle-count");
        attach_oracle(&dir, &full_oracle()).unwrap();
        let path = dir.join("oracle").join("single_vertex_write.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let first_line = text.lines().next().unwrap().to_string();
        std::fs::write(&path, first_line).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("has 1 oracle record(s) but manifest says 2"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_stray_oracle_file() {
        // An oracle file not named by the manifest would be dead, UNHASHED content in a bundle
        // that claims integrity — reject it.
        let dir = record_write_bundle_to_temp("synthrec-oracle-stray");
        attach_oracle(&dir, &full_oracle()).unwrap();
        std::fs::write(dir.join("oracle").join("ghost.jsonl"), "{}").unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unexpected oracle file") && msg.contains("ghost"), "got: {msg}");
        assert!(
            !msg.contains("interrupted"),
            "a v3 bundle's stray file is tampering, not an interrupted attach: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_names_the_repair_for_an_orphaned_pre_v3_oracle_dir() {
        // An interrupted attach leaves oracle/ files next to a still-v2 manifest; load() stays
        // strict but must point at the recovery instead of a bare corruption claim.
        let dir = record_write_bundle_to_temp("synthrec-oracle-orphan-msg");
        std::fs::create_dir_all(dir.join("oracle")).unwrap();
        std::fs::write(dir.join("oracle").join("w_alpha.jsonl"), "{}").unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unexpected oracle file") && msg.contains("interrupted"),
            "pre-v3 stray oracle files must name the repair path: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn attach_oracle_self_heals_an_interrupted_previous_attach() {
        // Simulate a crash between writing oracle/ files and upgrading the manifest: the bundle
        // is logically still v2, so a retrying attach must clear the orphaned directory and
        // succeed rather than brick on load()'s stray-file gate.
        let dir = record_write_bundle_to_temp("synthrec-oracle-selfheal");
        std::fs::create_dir_all(dir.join("oracle")).unwrap();
        std::fs::write(dir.join("oracle").join("single_vertex_write.jsonl"), "not even json")
            .unwrap();
        std::fs::write(dir.join("oracle").join("ghost.jsonl"), "{}").unwrap();
        let manifest = attach_oracle(&dir, &full_oracle()).unwrap();
        assert_eq!(manifest.format_version, RECORDING_FORMAT_VERSION_ORACLE_PREPARED);
        assert!(!dir.join("oracle").join("ghost.jsonl").exists(), "orphan cleared");
        let bundle = load(&dir).unwrap();
        assert_eq!(bundle.oracle["single_vertex_write"], vec![stats(1), stats(2)]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn re_recording_over_an_oracle_bundle_clears_the_stale_oracle() {
        // §6.3 lifecycle: `record` replaces the bundle wholesale. Re-recording into a directory
        // holding a v3 bundle must not leave the old oracle/ files behind — the fresh manifest is
        // v2 and can't reference them, so a stale directory would brick every subsequent load.
        let dir = record_write_bundle_to_temp("synthrec-oracle-rerecord");
        attach_oracle(&dir, &full_oracle()).unwrap();
        assert!(dir.join("oracle").is_dir(), "precondition: v3 bundle with oracle/");
        // Re-record (same shape as the helper, fresh corpus) into the SAME directory.
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let op = RecordedOp {
            key: OpKey::dynamic("single_vertex_write", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["CREATE (n:User)".to_string()],
        };
        let manifest = record_rendered(&spec, "g", &[op], 1, 8, &dir).unwrap();
        assert_eq!(manifest.format_version, RECORDING_FORMAT_VERSION_WRITES);
        assert!(!dir.join("oracle").exists(), "stale oracle/ must be cleared by record");
        load(&dir).expect("the re-recorded v2 bundle must load cleanly");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_pre_v3_manifest_declaring_an_oracle() {
        // The oracle field is manifest metadata (unhashed) — a v2 manifest declaring one is a
        // hand-edit, gated on the format version before anything else is read.
        let dir = record_write_bundle_to_temp("synthrec-oracle-v2decl");
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let bad = text.replacen("\"count\": 2", "\"oracle\": 1,\n      \"count\": 2", 1);
        assert_ne!(text, bad);
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("oracle bundles are format_version 3+"),
            "got: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_v3_bundle_with_no_oracle() {
        // v3 exists to carry the oracle: a v3 manifest without one should have been v2 — a
        // crafted version bump must not be a free pass.
        let dir = record_write_bundle_to_temp("synthrec-oracle-v3empty");
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let bad = text.replacen("\"format_version\": 2", "\"format_version\": 3", 1);
        assert_ne!(text, bad);
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("declares no outcome oracle"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_an_oracle_on_a_read_op() {
        // Craft a v3 READ bundle whose entry declares an oracle: the kind gate must fire (before
        // any hash work) — mutation outcomes exist only for writes.
        let (dir, _man) = record_to_temp(11);
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let bad = text
            .replacen("\"format_version\": 1", "\"format_version\": 3", 1)
            .replacen("\"count\":", "\"oracle\": 1,\n      \"count\":", 1);
        assert_ne!(text, bad);
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("oracle for read op"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_an_oracle_longer_than_the_corpus() {
        // Manifest declares more oracle records than the op has commands: replay would index past
        // the corpus. Craft it by editing an attached bundle's counts — the §6.3 exact-set pass
        // rejects it at manifest level (pre-hash).
        let dir = record_write_bundle_to_temp("synthrec-oracle-onlylong");
        attach_oracle(&dir, &full_oracle()).unwrap();
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let bad = text.replacen("\"oracle\": 2", "\"oracle\": 9", 1);
        assert_ne!(text, bad);
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("declares 9 oracle record(s) for 2 command(s)")
                && msg.contains("complete corpus"),
            "got: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_hash_valid_oracle_subset() {
        // A v3 bundle whose oracle covers only SOME eligible ops must be rejected even when the
        // workload_hash is recomputed to match (the duck exploit: strip one op's oracle, rehash).
        // Coverage is part of what "format v3" promises — the exact-set gate, not the hash, is
        // what stops a silent shrink.
        let dir = record_write_bundle_to_temp("synthrec-oracle-subset");
        attach_oracle(&dir, &full_oracle()).unwrap();
        let bundle = load(&dir).unwrap();
        // Strip single_vertex_update's oracle and REHASH so only the exact-set gate can object.
        let mut manifest = bundle.manifest.clone();
        for entry in &mut manifest.ops {
            if entry.name == "single_vertex_update" {
                entry.oracle = None;
            }
        }
        let mut hasher = WorkloadHasher::new(
            manifest.format_version,
            &manifest.generator_version,
            &manifest.dataset,
            &manifest.graph,
            manifest.corpus_seed,
        );
        for (phase, cypher) in &bundle.graph_statements {
            hasher.graph_record(phase.tag(), cypher);
        }
        for ((_, cyphers), entry) in bundle.commands.iter().zip(&manifest.ops) {
            hasher.op_header(&entry.name, cyphers.len(), entry.kind);
            for cypher in cyphers {
                hasher.command(cypher);
            }
            if entry.oracle.is_some() {
                for (seq, s) in bundle.oracle[&entry.name].iter().enumerate() {
                    hasher.oracle_record(seq, s);
                }
            }
        }
        manifest.workload_hash = hasher.finalize();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::remove_file(dir.join("oracle").join("single_vertex_update.jsonl")).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("oracle-eligible op 'single_vertex_update' carries no outcome oracle")
                && msg.contains("exact-set"),
            "got: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn replay_refuses_a_rehashed_v4_to_v2_downgrade_under_require_oracle() {
        // The two-sided blind spot: strip a v4 bundle's oracle entirely, declare it v2, and
        // REHASH under the v2 rules — the result is byte-indistinguishable from a legitimate
        // latency-tier recording (v2 hashes never covered oracle data), loads cleanly, and would
        // replay with `oracle_verified: None`. Only the operator's stated expectation can refuse
        // it: `--require-oracle`.
        use crate::synthetic::replay::{self, ReplayConfig};
        use crate::synthetic::CacheSelection;

        let dir = record_write_bundle_to_temp("synthrec-downgrade");
        attach_oracle(&dir, &full_oracle()).unwrap();
        let bundle = load(&dir).unwrap();

        // Downgrade: v2 format, no per-op oracle counts, no oracle/ dir, valid v2 hash.
        let mut manifest = bundle.manifest.clone();
        manifest.format_version = RECORDING_FORMAT_VERSION_WRITES;
        for entry in &mut manifest.ops {
            entry.oracle = None;
        }
        let mut hasher = WorkloadHasher::new(
            manifest.format_version,
            &manifest.generator_version,
            &manifest.dataset,
            &manifest.graph,
            manifest.corpus_seed,
        );
        for (phase, cypher) in &bundle.graph_statements {
            hasher.graph_record(phase.tag(), cypher);
        }
        for ((_, cyphers), entry) in bundle.commands.iter().zip(&manifest.ops) {
            hasher.op_header(&entry.name, cyphers.len(), entry.kind);
            for cypher in cyphers {
                hasher.command(cypher);
            }
        }
        manifest.workload_hash = hasher.finalize();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::remove_dir_all(dir.join("oracle")).unwrap();

        // The downgrade IS a legitimate v2 — the load hash gate cannot object…
        load(&dir).expect("a rehashed v3→v2 strip must be indistinguishable from a real v2");

        // …so the replay-side flag is the only refusal point.
        let config = ReplayConfig {
            recording_dir: dir.clone(),
            // Closed port: nothing must connect — both run() calls below fail offline or on
            // connect, never against a live server.
            endpoint: "falkor://127.0.0.1:1".to_string(),
            graph: None,
            load: true,
            samples: 5,
            warmup: 0,
            concurrency: vec![1],
            cache: CacheSelection::Cached,
            server_timeout_ms: 5_000,
            client_deadline_ms: 6_000,
            out: "unused.json".to_string(),
            server_image: None,
            label: None,
            require_oracle: true,
        };
        let err = replay::run(&config).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("carries no outcome oracle"), "got: {msg}");
        assert!(msg.contains("re-hashed oracle→v2 downgrade"), "got: {msg}");

        // Negative control: restore the oracle (v4 again) — the same flag now clears the gate
        // and the run proceeds to the connection attempt (no oracle complaint on the closed
        // port's error).
        attach_oracle(&dir, &full_oracle()).unwrap();
        let err = replay::run(&config).await.unwrap_err();
        let msg = format!("{err}");
        assert!(!msg.contains("oracle"), "v4 must clear the require-oracle gate, got: {msg}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_v3_bundle_loads_and_round_trips() {
        // Format v3's meaning is FROZEN (§6.4 duck review): a #267-era bundle — the seven-op
        // §6.3 exact set, no prepared phase — keeps loading under its own rule even though the
        // live registry now has nine eligible ops and mints v4.
        let dir = temp_bundle_dir("synthrec-legacy-v3");
        test_forge::forge_oracle_bundle(
            &dir,
            &test_forge::legacy_v3_ops(),
            false,
            RECORDING_FORMAT_VERSION_ORACLE,
        );
        let bundle = load(&dir).expect("a legacy seven-op v3 bundle must keep loading");
        assert_eq!(bundle.manifest.format_version, RECORDING_FORMAT_VERSION_ORACLE);
        assert_eq!(bundle.oracle.len(), 7, "the frozen legacy exact set");
        assert!(
            !bundle.graph_statements.iter().any(|(p, _)| *p == LoadPhase::Prepared),
            "v3 bundles predate the prepared phase"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_v3_bundle_with_a_prepared_phase() {
        // v3 predates §6.4: a hash-valid "v3" carrying the prepared phase is a crafted or
        // downgraded v4, not recorded history.
        let dir = temp_bundle_dir("synthrec-v3-prepared");
        test_forge::forge_oracle_bundle(
            &dir,
            &test_forge::legacy_v3_ops(),
            true,
            RECORDING_FORMAT_VERSION_ORACLE,
        );
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("predates the prepared state"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_v4_bundle_without_the_prepared_phase() {
        // v4 is defined by the prepared phase: all nine live-eligible ops covered, but the
        // prepared load phase stripped — a crafted/upgraded layout, rejected structurally.
        let nine: Vec<&str> = crate::synthetic::shapes::oracle_eligible_names()
            .into_iter()
            .collect();
        let dir = temp_bundle_dir("synthrec-v4-unprepared");
        test_forge::forge_oracle_bundle(
            &dir,
            &nine,
            false,
            RECORDING_FORMAT_VERSION_ORACLE_PREPARED,
        );
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("lacks the §6.4 prepared load phase"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_refuses_rehashed_cross_version_flips() {
        // A version flip on an otherwise-valid rehashed bundle is refused in BOTH directions:
        // the layout (prepared phase) and the per-version exact set disagree with the claimed
        // version even when the hash is internally consistent.
        // v3 → v4 upgrade: legacy seven-op layout claiming v4 lacks the prepared phase.
        let dir_up = temp_bundle_dir("synthrec-flip-up");
        test_forge::forge_oracle_bundle(
            &dir_up,
            &test_forge::legacy_v3_ops(),
            false,
            RECORDING_FORMAT_VERSION_ORACLE_PREPARED,
        );
        let msg = format!("{}", load(&dir_up).unwrap_err());
        assert!(msg.contains("lacks the §6.4 prepared load phase"), "got: {msg}");

        // v4 → v3 downgrade: nine-op prepared layout claiming v3 carries oracles for ops
        // outside the frozen legacy set (and a prepared phase v3 forbids).
        let nine: Vec<&str> = crate::synthetic::shapes::oracle_eligible_names()
            .into_iter()
            .collect();
        let dir_down = temp_bundle_dir("synthrec-flip-down");
        test_forge::forge_oracle_bundle(&dir_down, &nine, true, RECORDING_FORMAT_VERSION_ORACLE);
        let msg = format!("{}", load(&dir_down).unwrap_err());
        assert!(
            msg.contains("not oracle-eligible under format v3"),
            "got: {msg}"
        );

        std::fs::remove_dir_all(&dir_up).ok();
        std::fs::remove_dir_all(&dir_down).ok();
    }

    #[test]
    fn attach_oracle_rejects_a_bundle_without_the_prepared_phase() {
        // attach mints v4, and v4 requires the prepared phase — a stale preparedless write
        // bundle must be re-recorded, not upgraded into an unloadable state.
        let dir = temp_bundle_dir("synthrec-attach-unprepared");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let op = |name: &str, n: usize| RecordedOp {
            key: OpKey::dynamic(name, QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: (0..n)
                .map(|i| format!("CYPHER x={} CREATE (n:User {{id:$x}})", i))
                .collect(),
        };
        record_rendered(
            &spec,
            "g",
            &[op("single_vertex_write", 2), op("single_vertex_update", 3)],
            1,
            8,
            &dir,
        )
        .unwrap();
        let err = attach_oracle(&dir, &full_oracle()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("lacks the §6.4 prepared load phase"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_a_prepared_phase_on_a_read_bundle() {
        // The prepared state exists solely for the write shapes; a read bundle carrying one is
        // crafted (the recorder refuses to write it — see
        // `record_rejects_a_mismatched_extra_load_block`).
        let (dir, _man) = record_to_temp(21);
        let graph_path = dir.join("graph.jsonl");
        let n_stmts = std::fs::read_to_string(&graph_path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        let rec = GraphRecord {
            seq: n_stmts,
            phase: "prepared".to_string(),
            cypher: "MATCH (u:User) SET u.crafted = 1".to_string(),
        };
        let mut text = std::fs::read_to_string(&graph_path).unwrap();
        text.push_str(&serde_json::to_string(&rec).unwrap());
        text.push('\n');
        std::fs::write(&graph_path, text).unwrap();
        test_forge::rehash_bundle(&dir);
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("records no write ops"), "got: {msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_an_oracle_on_a_non_eligible_write_op() {
        // Mixed bundle (one eligible + one custom write op): attach the valid exact-set oracle,
        // then hand-edit the manifest to declare an oracle on the non-eligible op — the §6.3
        // exact-set pass must reject it at manifest level (a padded oracle outside the
        // eligible set is as much a coverage lie as a shrunken one).
        let dir = temp_bundle_dir("synthrec-oracle-noneligible");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let op = |name: &str, n: usize| RecordedOp {
            key: OpKey::dynamic(name, QueryType::Write),
            result_gated: false,
            budget: RecordedBudget::default(),
            capability: None,
            commands: (0..n).map(|i| format!("CYPHER x={} CREATE (:User)", i)).collect(),
        };
        record_rendered_with_prepared(
            &spec,
            "g",
            &[op("single_vertex_write", 2), op("w_custom", 1)],
            1,
            8,
            &dir,
        )
        .unwrap();
        let mut oracle = BTreeMap::new();
        oracle.insert("single_vertex_write".to_string(), vec![stats(1), stats(2)]);
        attach_oracle(&dir, &oracle).unwrap();
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let bad = text.replacen("\"count\": 1", "\"oracle\": 1,\n      \"count\": 1", 1);
        assert_ne!(text, bad);
        std::fs::write(&path, bad).unwrap();
        let err = load(&dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("'w_custom'") && msg.contains("not oracle-eligible"),
            "got: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn attach_oracle_rejects_invalid_attachments() {
        let dir = record_write_bundle_to_temp("synthrec-oracle-invalid");
        // Empty capture.
        let err = attach_oracle(&dir, &BTreeMap::new()).unwrap_err();
        assert!(format!("{err}").contains("empty capture"), "got: {err}");
        // Unknown op alongside the exact set: the bundle does not record it.
        let mut unknown = full_oracle();
        unknown.insert("nope".to_string(), vec![stats(1)]);
        let err = attach_oracle(&dir, &unknown).unwrap_err();
        assert!(format!("{err}").contains("does not record"), "got: {err}");
        // Missing eligible op (§6.3 exact-set rule: no subsets).
        let mut partial = full_oracle();
        partial.remove("single_vertex_update");
        let err = attach_oracle(&dir, &partial).unwrap_err();
        assert!(
            format!("{err}").contains("no oracle captured for oracle-eligible op"),
            "got: {err}"
        );
        // Empty outcome vector (an incomplete corpus).
        let mut empty = full_oracle();
        empty.insert("single_vertex_write".to_string(), Vec::new());
        let err = attach_oracle(&dir, &empty).unwrap_err();
        assert!(format!("{err}").contains("has 0 outcome(s)"), "got: {err}");
        // More outcomes than commands.
        let mut long = full_oracle();
        long.insert("single_vertex_write".to_string(), vec![stats(1), stats(2), stats(3)]);
        let err = attach_oracle(&dir, &long).unwrap_err();
        assert!(format!("{err}").contains("has 3 outcome(s)"), "got: {err}");
        // The failed attempts must not have corrupted the bundle.
        load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn attach_oracle_rejects_a_read_op_and_an_already_oracled_bundle() {
        // Read op: a v1 read bundle's ops have no mutation outcome to attach (and no read op is
        // oracle-eligible).
        let (read_dir, man) = record_to_temp(13);
        let mut on_read = BTreeMap::new();
        on_read.insert(man.ops[0].name.clone(), vec![stats(1)]);
        let err = attach_oracle(&read_dir, &on_read).unwrap_err();
        assert!(format!("{err}").contains("not oracle-eligible"), "got: {err}");
        std::fs::remove_dir_all(&read_dir).ok();
        // Double attach: the second must refuse (re-record instead).
        let dir = record_write_bundle_to_temp("synthrec-oracle-double");
        attach_oracle(&dir, &full_oracle()).unwrap();
        let err = attach_oracle(&dir, &full_oracle()).unwrap_err();
        assert!(format!("{err}").contains("already carries"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn attach_oracle_heals_an_orphaned_oracle_from_an_interrupted_attach() {
        // A crash between attach's oracle/ write and its manifest rewrite leaves a v2 manifest
        // with an oracle/ directory next to it. The retry must clear the orphan and succeed.
        let dir = record_write_bundle_to_temp("synthrec-oracle-orphan");
        let orphan = dir.join("oracle");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("single_vertex_write.jsonl"), b"junk\n").unwrap();
        let man = attach_oracle(&dir, &full_oracle()).unwrap();
        assert_eq!(man.format_version, RECORDING_FORMAT_VERSION_ORACLE_PREPARED);
        load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v2_write_bundles_serialize_no_oracle_field() {
        // Pre-oracle byte-identity (§7.5): a plain write bundle's manifest must not mention the
        // oracle at all — v1/v2 bundles and their hashes are unchanged by the v3 feature.
        let dir = record_write_bundle_to_temp("synthrec-oracle-absent");
        let text = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert!(!text.contains("oracle"), "v2 manifest must omit the oracle field: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oracle_record_serde_is_strict() {
        // Round-trip.
        let rec = OracleRecord {
            seq: 3,
            stats: stats(1),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: OracleRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
        // Unknown fields are rejected (a typo'd counter must not silently read as 0)…
        let unknown = r#"{"seq":0,"stats":{"nodes_created":1,"nodes_deleted":0,"relationships_created":0,"relationships_deleted":0,"properties_set":0,"properties_removed":0,"labels_removed":0,"labels_added":9}}"#;
        assert!(serde_json::from_str::<OracleRecord>(unknown).is_err());
        // …and every counter is required (a truncated record must not default to 0).
        let missing = r#"{"seq":0,"stats":{"nodes_created":1}}"#;
        assert!(serde_json::from_str::<OracleRecord>(missing).is_err());
    }

    #[test]
    fn oracle_hash_parts_are_order_and_value_sensitive() {
        // The hasher's oracle framing: same records in a different order, or one changed counter,
        // must hash differently; identical streams must agree.
        let hash = |records: &[(usize, MutationStats)]| {
            let mut h = WorkloadHasher(Sha256::new(), RECORDING_FORMAT_VERSION_ORACLE);
            for (seq, s) in records {
                h.oracle_record(*seq, s);
            }
            h.finalize()
        };
        let a = [(0, stats(1)), (1, stats(2))];
        let b = [(0, stats(2)), (1, stats(1))];
        let c = [(0, stats(1)), (1, stats(2))];
        assert_eq!(hash(&a), hash(&c));
        assert_ne!(hash(&a), hash(&b));
    }
}
