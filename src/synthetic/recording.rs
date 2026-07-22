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
use crate::synthetic::dataset::{load_statements, DatasetSpec, LoadPhase, GENERATOR_VERSION};
use crate::synthetic::OpName;
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
    pub count: usize,
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
    /// Each recorded op's ordered commands, in the manifest's op order.
    pub commands: Vec<(OpName, Vec<String>)>,
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

/// De-duplicate ops preserving first-occurrence order (matching how a run executes them).
fn dedup_ops(ops: &[OpName]) -> Vec<OpName> {
    let mut seen = std::collections::BTreeSet::new();
    ops.iter()
        .copied()
        .filter(|op| seen.insert(op.as_str()))
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

/// Record a workload bundle to `out_dir` (created if absent). **Offline** — no server is contacted.
///
/// Writes `graph.jsonl` (the [`load_statements`] for `dataset`), `commands/<op>.jsonl` for each op,
/// and `manifest.json` with the [`Manifest::workload_hash`]. `ops` are de-duplicated (first
/// occurrence wins); **write ops are rejected** (v1 records read ops only). Returns the manifest.
pub fn record(
    dataset: &DatasetSpec,
    graph: &str,
    ops: &[OpName],
    corpus_seed: u64,
    batch_size: usize,
    out_dir: &Path,
) -> BenchmarkResult<Manifest> {
    dataset.validate()?;
    if batch_size == 0 {
        return Err(OtherError("record batch_size must be greater than 0".to_string()));
    }
    let ops = dedup_ops(ops);
    if ops.is_empty() {
        return Err(OtherError(
            "no operations to record — pass at least one read --op".to_string(),
        ));
    }
    if let Some(op) = ops.iter().find(|op| spec(**op).kind == QueryType::Write) {
        return Err(OtherError(format!(
            "recording write op '{}' is not supported yet (v1 records read ops only)",
            op.as_str()
        )));
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

    // graph.jsonl — streamed straight from the lazy statement iterator (one batch in memory).
    let graph_path = out_dir.join("graph.jsonl");
    {
        let mut w = BufWriter::new(create_file(&graph_path)?);
        for (seq, (phase, stmt)) in load_statements(dataset, batch_size).enumerate() {
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
        let cyphers = render_commands(*op, dataset, corpus_seed)?;
        hasher.op_header(op.as_str(), cyphers.len());
        let path = commands_dir.join(format!("{}.jsonl", op.as_str()));
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
            name: op.as_str().to_string(),
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
    for entry in &manifest.ops {
        let op = OpName::from_tag(&entry.name)
            .ok_or_else(|| OtherError(format!("manifest names unknown op '{}'", entry.name)))?;
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
            assert_eq!(*cyphers, render_commands(*op, &spec, manifest.corpus_seed).unwrap());
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
