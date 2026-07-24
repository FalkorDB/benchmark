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
//!
//! The [`Manifest::workload_hash`] is a **length-framed** SHA-256 over the header, every graph
//! record, and every op's commands (in order) — so any edit to the graph *or* the commands is
//! detected on [`load`] (the integrity gate), and two runs are only compared when it matches.
//!
//! Recording is **offline** (a pure function of the spec + seed) — no server is contacted.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::queries_repository::QueryType;
use crate::synthetic::catalog::spec;
use crate::synthetic::dataset::{
    fixture_statements, load_statements, DatasetSpec, LoadPhase, GENERATOR_VERSION,
};
use crate::synthetic::{OpKey, OpName};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// On-disk bundle format version. Bumped when the bundle layout changes incompatibly.
pub const RECORDING_FORMAT_VERSION: u32 = 1;

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
    /// (v1 records reads only), and is **not** folded into the [`Manifest::workload_hash`].
    #[serde(default = "default_op_kind")]
    pub kind: QueryType,
    /// Whether this op's result is compared across the A/B (record-once / replay-verbatim) gate.
    /// `true` (the default, and the value for every built-in catalog op) means replay computes and
    /// gates a `result_digest`; `false` marks the op **result-N/A** — still recorded and timed, but
    /// its result is *not* gated, for shapes whose result set isn't byte-stable (LIMIT-without-
    /// ORDER, top-k, float scores — design §3.2 / Decision 4). Like [`Self::kind`] it is **not**
    /// folded into the [`Manifest::workload_hash`] (it's replay-gating policy, not workload content)
    /// and defaults to `true` for bundles written before this field existed.
    #[serde(default = "default_result_gated")]
    pub result_gated: bool,
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
    pub kind: String,
    pub cypher: String,
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
}

/// Length-framed workload hasher. Every part is prefixed with its byte length (u64 LE) before the
/// bytes, so no concatenation of parts can collide with a different split (e.g. `["ab","c"]` and
/// `["a","bc"]` hash differently). Record (streaming) and [`load`] (from memory) feed it in the
/// same order, so they agree iff the content matches.
struct WorkloadHasher(Sha256);

impl WorkloadHasher {
    /// Start a hasher seeded with the bundle header (everything but the graph/command bodies).
    fn new(
        format_version: u32,
        generator_version: &str,
        dataset: &DatasetKnobs,
        graph: &str,
        corpus_seed: u64,
    ) -> Self {
        let mut h = WorkloadHasher(Sha256::new());
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

    /// Feed one operation header (name + command count, tagged `O`).
    fn op_header(
        &mut self,
        name: &str,
        count: usize,
    ) {
        self.part(b"O");
        self.part(name.as_bytes());
        self.part(&(count as u64).to_le_bytes());
    }

    /// Feed one measured command (tagged `C`).
    fn command(
        &mut self,
        cypher: &str,
    ) {
        self.part(b"C");
        self.part(cypher.as_bytes());
    }

    fn finalize(self) -> String {
        format!("sha256:{:x}", self.0.finalize())
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
/// then delegates to [`record_rendered`]; **write ops are rejected** (v1 records read ops only).
pub fn record(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[OpName],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    // Reject writes up-front — before rendering — so we never build a write op's corpus (v1 records
    // reads only). [`record_rendered`] re-checks by kind for string-keyed callers.
    if let Some(op) = ops.iter().find(|op| spec(**op).kind == QueryType::Write) {
        return Err(OtherError(format!(
            "recording write op '{}' is not supported yet (v1 records read ops only)",
            op.as_str()
        )));
    }
    let mut recorded = Vec::with_capacity(ops.len());
    for &op in ops {
        recorded.push(RecordedOp {
            key: OpKey::from(op),
            // Every built-in catalog op projects byte-stable scalars, so its result is gated.
            result_gated: true,
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
/// occurrence wins); **write ops are rejected** (v1 records read ops only). Returns the manifest.
pub fn record_rendered(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[RecordedOp],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    record_rendered_impl(dataset, graph, ops, corpus_seed, batch_size, out_dir, false)
}

/// Like [`record_rendered`], but also appends the post-load [`fixture_statements`] (the fulltext +
/// vector index DDL and their deterministic seed data) to `graph.jsonl`, folded into the
/// [`Manifest::workload_hash`]. Used when a recording includes the FixtureDependent read shapes so
/// every engine replays the identical fulltext/vector fixture (record-once → replay-verbatim). A
/// bundle written this way stays byte-identical across engines; the fixture statements are constant
/// and idempotent (design §3.4).
pub fn record_rendered_with_fixture(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[RecordedOp],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    record_rendered_impl(dataset, graph, ops, corpus_seed, batch_size, out_dir, true)
}

/// Shared body of [`record_rendered`] / [`record_rendered_with_fixture`]. When `include_fixture` is
/// set, the fixture statements are streamed into `graph.jsonl` after the base load statements and
/// hashed in the same order, so the two entry points differ only by whether the fixture is present.
fn record_rendered_impl(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[RecordedOp],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
    include_fixture: bool,
) -> BenchmarkResult<Manifest> {
    dataset.validate()?;
    if batch_size == 0 {
        return Err(OtherError("record batch_size must be greater than 0".to_string()));
    }
    // Reject writes on the *original* ops (before dedup) so a duplicate-name write can't be dropped
    // by dedup and slip through, and validate every name up-front (a name becomes a file stem).
    for op in ops {
        validate_op_name(op.key.name())?;
        if op.key.kind() == QueryType::Write {
            return Err(OtherError(format!(
                "recording write op '{}' is not supported yet (v1 records read ops only)",
                op.key.name()
            )));
        }
    }
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

    let mut hasher = WorkloadHasher::new(
        RECORDING_FORMAT_VERSION,
        GENERATOR_VERSION,
        &knobs,
        graph,
        corpus_seed,
    );

    // graph.jsonl — streamed straight from the lazy statement iterator (one batch in memory). When
    // a fixture is requested, its statements follow the base load statements in the same stream, so
    // they are written and hashed in that exact order (index → nodes → edges → fixture).
    let graph_path = out_dir.join("graph.jsonl");
    {
        let mut w = BufWriter::new(create_file(&graph_path)?);
        let fixture = fixture_statements().take(if include_fixture { usize::MAX } else { 0 });
        let stmts = load_statements(dataset, batch_size).chain(fixture);
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
        hasher.op_header(name, cyphers.len());
        let path = commands_dir.join(format!("{}.jsonl", name));
        let mut w = BufWriter::new(create_file(&path)?);
        for (seq, cypher) in cyphers.iter().enumerate() {
            hasher.command(cypher);
            let rec = CommandRecord {
                seq,
                kind: "read".to_string(),
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
            count: cyphers.len(),
        });
    }

    let manifest = Manifest {
        format_version: RECORDING_FORMAT_VERSION,
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
    if manifest.format_version != RECORDING_FORMAT_VERSION {
        return Err(OtherError(format!(
            "unsupported recording format_version {} (this build expects {})",
            manifest.format_version, RECORDING_FORMAT_VERSION
        )));
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
        commands.push((op, recs.into_iter().map(|r| r.cypher).collect::<Vec<_>>()));
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
        hasher.op_header(&entry.name, cyphers.len());
        for cypher in cyphers {
            hasher.command(cypher);
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
    })
}

impl Bundle {
    /// The dataset spec the bundle was recorded from.
    pub fn spec(&self) -> DatasetSpec {
        self.manifest.dataset.spec()
    }
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
        assert!(format!("{}", err).contains("write op"), "got: {err}");
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
    fn load_canonicalizes_builtin_names_to_named_ops() {
        // Built-in read ops recorded via `record` load back as `Named` keys (canonicalized by name),
        // so the built-in salt/kind is preserved across a record→load round-trip.
        let (dir, _man) = record_to_temp(11);
        let bundle = load(&dir).unwrap();
        assert!(bundle.commands.iter().all(|(k, _)| k.is_named()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_rejects_write_kind_ops() {
        let dir = temp_bundle_dir("synthrec-dynwrite");
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("bulk_insert", QueryType::Write),
            result_gated: true,
            commands: vec!["CYPHER x=1 CREATE (n:User {id:$x})".to_string()],
        }];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        assert!(format!("{}", err).contains("write op"), "got: {err}");
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
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("a_read", QueryType::Read),
                result_gated: true,
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
            commands: vec!["CYPHER  RETURN 1".to_string()],
        }];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        assert!(format!("{err}").contains("unsafe operation name"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_rendered_rejects_a_write_hidden_behind_a_duplicate_read() {
        // The write check runs on the original ops (before dedup), so a later same-named write is
        // caught rather than dropped by first-occurrence dedup.
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
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("dup", QueryType::Write),
                result_gated: true,
                commands: vec!["CYPHER  CREATE (n)".to_string()],
            },
        ];
        let err = record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap_err();
        assert!(format!("{err}").contains("write op"), "got: {err}");
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
                commands: vec!["CYPHER  RETURN 1".to_string()],
            },
            RecordedOp {
                key: OpKey::dynamic("na_read", QueryType::Read),
                result_gated: false,
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
    }

    #[test]
    fn workload_hash_is_length_framed() {
        // ["ab","c"] and ["a","bc"] must not collide — the length prefix disambiguates.
        let mut h1 = WorkloadHasher(Sha256::new());
        h1.part(b"ab");
        h1.part(b"c");
        let mut h2 = WorkloadHasher(Sha256::new());
        h2.part(b"a");
        h2.part(b"bc");
        assert_ne!(h1.finalize(), h2.finalize());
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
}
