use crate::queries_repository::{QueryCoverageProfile, QueryType};
use crate::scenario::Vendor;
use crate::synthetic::{CacheSelection, OpName, Tier};
use clap::{Parser, Subcommand};
use clap_complete::Shell;

/// A `--op` value: either a single operation, or the magic `all` / `*` meaning **every** read op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpSelector {
    /// The magic `all` / `*` — every read operation.
    All,
    /// One named operation.
    One(OpName),
}

/// Parse one `--op` value: `all` or `*` → [`OpSelector::All`]; otherwise a valid operation name.
fn parse_op_selector(s: &str) -> Result<OpSelector, String> {
    match s {
        "all" | "*" => Ok(OpSelector::All),
        name => OpName::from_tag(name).map(OpSelector::One).ok_or_else(|| {
            format!("unknown operation '{name}' — use an operation name, or 'all' / '*' for every read op")
        }),
    }
}

/// Expand `--op` selectors to concrete operations. [`OpSelector::All`] contributes every read op
/// (in canonical order); explicitly named ops are kept too, so a write op listed alongside `all`
/// (e.g. `--op all,create_node` on `run`) is **not** silently dropped. Duplicates are removed,
/// preserving first-occurrence order. Empty input stays empty (no `--op` given).
pub fn expand_op_selectors(selectors: &[OpSelector]) -> Vec<OpName> {
    let mut ops: Vec<OpName> = Vec::new();
    let push_unique = |op: OpName, ops: &mut Vec<OpName>| {
        if !ops.contains(&op) {
            ops.push(op);
        }
    };
    for selector in selectors {
        match selector {
            OpSelector::All => {
                for op in OpName::all_reads() {
                    push_unique(op, &mut ops);
                }
            }
            OpSelector::One(op) => push_unique(*op, &mut ops),
        }
    }
    ops
}

/// A clap value parser for `--op` that parses [`OpSelector`] via [`parse_op_selector`] while still
/// advertising its **possible values** (operation tags plus `all` / `*`) to `--help` and to
/// shell-completion (`GenerateAutoComplete`) — which a bare function `value_parser` cannot do.
/// When `reads_only`, it advertises and accepts read ops only (used by `record`, which cannot
/// record write ops), so an unrecordable write op is rejected at parse time instead of mid-run.
#[derive(Clone, Copy)]
struct OpSelectorValueParser {
    reads_only: bool,
}

impl OpSelectorValueParser {
    /// Accept every operation (reads + writes) — used by `run`.
    const fn all_ops() -> Self {
        Self { reads_only: false }
    }

    /// Accept read operations only — used by `record` (write ops aren't recordable).
    const fn reads_only() -> Self {
        Self { reads_only: true }
    }

    /// Build an `InvalidValue` clap error that keeps `parse_op_selector`'s actionable message
    /// (surfaced as a suggestion) alongside the standard invalid-arg/-value context.
    fn invalid_value(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        raw: String,
        message: String,
    ) -> clap::Error {
        let mut err = clap::Error::new(clap::error::ErrorKind::InvalidValue).with_cmd(cmd);
        if let Some(arg) = arg {
            err.insert(
                clap::error::ContextKind::InvalidArg,
                clap::error::ContextValue::String(arg.to_string()),
            );
        }
        err.insert(
            clap::error::ContextKind::InvalidValue,
            clap::error::ContextValue::String(raw),
        );
        err.insert(
            clap::error::ContextKind::Suggested,
            clap::error::ContextValue::StyledStrs(vec![message.into()]),
        );
        err
    }
}

impl clap::builder::TypedValueParser for OpSelectorValueParser {
    type Value = OpSelector;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let raw = clap::builder::StringValueParser::new().parse_ref(cmd, arg, value)?;
        let selector =
            parse_op_selector(&raw).map_err(|msg| self.invalid_value(cmd, arg, raw.clone(), msg))?;
        if self.reads_only {
            if let OpSelector::One(op) = &selector {
                if op.kind() == QueryType::Write {
                    return Err(self.invalid_value(
                        cmd,
                        arg,
                        raw,
                        format!(
                            "'{}' is a write op — recording supports read ops only (use 'all' for every read op)",
                            op.as_str()
                        ),
                    ));
                }
            }
        }
        Ok(selector)
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        let ops = if self.reads_only {
            OpName::all_reads()
        } else {
            OpName::all().to_vec()
        };
        let values: Vec<clap::builder::PossibleValue> = ops
            .iter()
            .map(|op| clap::builder::PossibleValue::new(op.as_str()))
            .chain([
                clap::builder::PossibleValue::new("all"),
                clap::builder::PossibleValue::new("*"),
            ])
            .collect();
        Some(Box::new(values.into_iter()))
    }
}

#[derive(Parser, Debug)]
#[command(name = "benchmark", version, about="falkor benchmark tool", long_about = None, arg_required_else_help(true), propagate_version(true))]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(arg_required_else_help = true)]
    GenerateAutoComplete { shell: Shell },
    #[command(arg_required_else_help = true)]
    #[command(about = "load data into the database")]
    Load {
        #[arg(short, long, value_enum)]
        vendor: Vendor,
        #[arg(short, long, value_enum)]
        size: crate::scenario::Size,
        #[arg(
            short,
            long,
            required = false,
            default_value_t = false,
            default_missing_value = "true",
            help = "execute clear -f before"
        )]
        force: bool,
        #[arg(
            short,
            long,
            required = false,
            default_value_t = false,
            default_missing_value = "true",
            help = "only load the data from the cache and iterate over it, show how much time it takes, do not send it to the server"
        )]
        dry_run: bool,
        #[arg(
            short,
            long,
            required = false,
            default_value_t = 1000,
            help = "number of cypher commands to execute in a single batch"
        )]
        batch_size: usize,
        #[arg(
            short,
            long,
            required = false,
            help = "endpoint for external database connection (e.g., falkor://127.0.0.1:6379)"
        )]
        endpoint: Option<String>,
        #[arg(
            long,
            value_enum,
            required = false,
            default_value_t = QueryCoverageProfile::Baseline,
            help = "query coverage profile used to decide if post-phase fixture/index setup should run"
        )]
        query_profile: QueryCoverageProfile,
    },
    #[command(
        about = "generate a set of queries and store them in a file to be used with the run command"
    )]
    GenerateQueries {
        #[arg(short, long, value_enum)]
        vendor: Vendor,
        #[arg(short, long, value_enum)]
        size: usize,
        #[arg(short, long, value_enum)]
        dataset: crate::scenario::Size,
        #[arg(
            short,
            long,
            required = false,
            default_missing_value = "queries.json",
            help = "name of json file to save the queries"
        )]
        name: String,
        #[arg(
            short,
            long,
            value_parser = parse_write_ratio,
            required = true,
            help = "the write ratio of the queries (0.0 - 1.0)"
        )]
        write_ratio: f32,
        #[arg(
            long,
            default_value_t = true,
            action = clap::ArgAction::Set,
            help = "enable the algo_pagerank_summary query in generated workloads"
        )]
        enable_algo_pagerank: bool,
        #[arg(
            long,
            default_value_t = true,
            action = clap::ArgAction::Set,
            help = "enable the algo_max_flow_single_pair query in generated workloads"
        )]
        enable_algo_max_flow: bool,
        #[arg(
            long,
            default_value_t = true,
            action = clap::ArgAction::Set,
            help = "enable the algo_msf_summary query in generated workloads"
        )]
        enable_algo_msf: bool,
        #[arg(
            long,
            default_value_t = true,
            action = clap::ArgAction::Set,
            help = "enable the algo_harmonic_summary query in generated workloads"
        )]
        enable_algo_harmonic: bool,
        #[arg(
            long,
            value_enum,
            required = false,
            default_value_t = QueryCoverageProfile::Baseline,
            help = "query coverage profile to generate (baseline, extended-core, fixture-dependent)"
        )]
        query_profile: QueryCoverageProfile,
    },

    #[command(
        about = "run the queries generated by the GenerateQueries command against the chosen vendor"
    )]
    Run {
        #[arg(short, long, value_enum)]
        vendor: Vendor,
        #[arg(
            short,
            long,
            required = false,
            default_value_t = 1,
            default_missing_value = "1",
            help = "parallelism level"
        )]
        parallel: usize,
        #[arg(
            short,
            long,
            required = false,
            default_missing_value = "queries.json",
            help = "name of json file to load the queries from"
        )]
        name: String,
        #[arg(
            short,
            long,
            required = true,
            help = "the rate of messages that sent to the server (messages per second)"
        )]
        mps: usize,
        #[arg(
            short,
            long,
            required = false,
            help = "simulate the benchmark without sending the messages to the server, the value the process time in milliseconds"
        )]
        simulate: Option<usize>,
        #[arg(
            short,
            long,
            required = false,
            help = "endpoint for external database connection (e.g., falkor://127.0.0.1:6379)"
        )]
        endpoint: Option<String>,
        #[arg(
            long,
            required = false,
            help = "base directory to write detailed per-vendor run results (will create <results-dir>/<vendor>/...). Defaults to Results-YYMMDD-HH:MM"
        )]
        results_dir: Option<String>,
    },
    #[command(about = "aggregate per-vendor run results into UI summary JSON files")]
    Aggregate {
        #[arg(
            long,
            required = true,
            help = "run results directory (contains subfolders: falkor/ neo4j/ memgraph/)"
        )]
        results_dir: String,
        #[arg(
            long,
            required = false,
            default_value = "ui/public/summaries",
            help = "directory to write UI summary JSON files"
        )]
        out_dir: String,
    },

    #[command(
        about = "aggregate aws-tests/ FalkorDB runs (e.g. graviton vs intel) into a UI summary JSON file"
    )]
    AggregateAwsTests {
        #[arg(
            long,
            required = false,
            default_value = "aws-tests",
            help = "directory containing subfolders with {meta.json,metrics.prom} (e.g. aws-tests/falkor-r8g-2xl/)"
        )]
        aws_tests_dir: String,
        #[arg(
            long,
            required = false,
            default_value = "ui/public/summaries/aws_tests_falkor_graviton_vs_intel.json",
            help = "output path for the UI summary JSON file"
        )]
        out_path: String,
    },

    #[command(
        about = "Run each generated Memgraph query type once against a Memgraph endpoint to detect failing queries"
    )]
    DebugMemgraphQueries {
        #[arg(short, long, value_enum)]
        dataset: crate::scenario::Size,
        #[arg(
            short,
            long,
            help = "endpoint for external Memgraph (e.g., bolt://127.0.0.1:7687)",
            required = true
        )]
        endpoint: String,
        #[arg(
            short,
            long,
            default_value = "small-readonly-memgraph",
            help = "name of json file to load the generated Memgraph queries from"
        )]
        name: String,
    },

    #[command(
        about = "synthetic per-operation latency probe (measures server + total time in isolation)"
    )]
    Synthetic {
        #[command(subcommand)]
        command: SyntheticCommands,
    },
}

/// Subcommands of `benchmark synthetic`.
// The `Run` variant carries many optional CLI knobs; this subcommand enum is parsed once at
// startup, so the size gap versus the unit `ListOps` variant doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum SyntheticCommands {
    #[command(about = "run the per-operation latency/throughput probe over one or more read or write operations")]
    Run {
        #[arg(
            long = "config",
            help = "path to a synthetic-bench.toml config (auto-detected in the CWD if present); CLI flags override it"
        )]
        config: Option<String>,
        #[arg(long, help = "FalkorDB endpoint (default falkor://127.0.0.1:6379)")]
        endpoint: Option<String>,
        #[arg(long, help = "graph key to measure against (default falkor)")]
        graph: Option<String>,
        #[arg(
            long = "op",
            value_parser = OpSelectorValueParser::all_ops(),
            value_delimiter = ',',
            num_args = 1..,
            help = "operation(s) to measure; repeatable and comma-separated (e.g. --op match_by_index,expand_1_hop). Use --op all (or --op '*') for every read op. Overrides the config's operations."
        )]
        ops: Vec<OpSelector>,
        #[arg(
            long,
            conflicts_with = "ops",
            help = "measure every read operation (same as --op all; mutually exclusive with --op)"
        )]
        all_reads: bool,
        #[arg(
            long,
            value_enum,
            conflicts_with_all = ["ops", "all_reads"],
            help = "measure a coverage tier: `core` (small per-PR read subset) or `full` (every read op; same as --all-reads, run nightly/on-demand). Mutually exclusive with --op/--all-reads."
        )]
        tier: Option<Tier>,
        #[arg(long, help = "number of measured invocations (default 1000)")]
        samples: Option<usize>,
        #[arg(long, help = "number of warm-up invocations, discarded (default 200)")]
        warmup: Option<usize>,
        #[arg(
            long = "concurrency",
            value_delimiter = ',',
            num_args = 1..,
            help = "concurrency levels to sweep (closed-loop workers C), repeatable/comma-separated (e.g. --concurrency 1,4,16,32). Default 1,2,4,8,16,32."
        )]
        concurrency: Vec<usize>,
        #[arg(
            long = "reset-every",
            help = "write-op reset cadence: every N ops each worker's scratch is reset (untimed) to bound write drift to one sawtooth window. Ignored by read ops. Default 50000."
        )]
        reset_every: Option<usize>,
        #[arg(
            long,
            help = "seed for the dataset and the per-operation corpora (same seed ⇒ identical workload; default 0)"
        )]
        seed: Option<u64>,
        #[arg(
            long,
            value_enum,
            help = "plan-cache condition: cached, uncached, or both (default both)"
        )]
        cache: Option<CacheSelection>,
        #[arg(
            long,
            help = "FalkorDB server-side per-query timeout in ms (default 5000)"
        )]
        server_timeout_ms: Option<i64>,
        #[arg(long, help = "client-side deadline per query in ms (default 6000)")]
        client_deadline_ms: Option<u64>,
        #[arg(
            long,
            help = "path to write the JSON report (default synthetic-report.json)"
        )]
        out: Option<String>,
        #[arg(
            long,
            env = "FALKOR_SERVER_IMAGE",
            help = "operator-supplied server image identity (e.g. falkordb/falkordb:v4.2.1@sha256:...), recorded verbatim"
        )]
        server_image: Option<String>,
        #[arg(
            long,
            help = "display name for this run (e.g. pr / main / 'release 1.2.3'), recorded into the report and used as the column header in report --diff/--regression"
        )]
        label: Option<String>,
        #[arg(
            long,
            help = "GENERATE a reproducible dataset into --graph before measuring. DESTRUCTIVE: drops and rewrites the graph. Requires --nodes/--edges (or config)."
        )]
        generate: bool,
        #[arg(long, help = "dataset node count (with --generate)")]
        nodes: Option<usize>,
        #[arg(long, help = "dataset edge count, must be >= nodes (with --generate)")]
        edges: Option<usize>,
        #[arg(
            long = "recording",
            help = "measure a RECORDED workload bundle (from `synthetic record`) instead of generating/probing: loads the recorded graph, then measures the recorded commands across --concurrency + --cache. Conflicts with --config/--generate/--op/--all-reads/--tier/--nodes/--edges/--seed."
        )]
        recording: Option<String>,
        #[arg(
            long = "no-load",
            requires = "recording",
            help = "with --recording: skip loading the recorded graph, only count-verify the already-loaded graph (load-once / run-many)."
        )]
        no_load: bool,
        #[arg(
            long = "require-oracle",
            requires = "recording",
            help = "with --recording: refuse to measure a write bundle that carries no outcome oracle (recording format < v3). Guards against re-hashed oracle-to-v2 downgrades; errors on read bundles (reads have no oracle)."
        )]
        require_oracle: bool,
        #[arg(
            long = "paired-endpoint",
            requires = "recording",
            help = "with --recording (READ bundles only): measure the bundle against this SECOND endpoint too, INTERLEAVED per cell — each op's cache-mode x concurrency cell runs on the primary endpoint then immediately here (A,B,A,B,...), so both sides of every per-op comparison share the same environment window. Both endpoints are set up identically (own graph load, reference pass, digests); two complete standard reports are written (--out / --paired-out) that work unchanged with `report --diff`/`--regression`. Refused for write bundles (their per-cell resets/restores don't interleave safely)."
        )]
        paired_endpoint: Option<String>,
        #[arg(
            long = "paired-graph",
            requires = "paired_endpoint",
            help = "with --paired-endpoint: the second side's graph key (default: same resolution as --graph). Give side B its own graph to pair two graphs on ONE server (an A/A self-check)."
        )]
        paired_graph: Option<String>,
        #[arg(
            long = "paired-out",
            requires = "paired_endpoint",
            help = "with --paired-endpoint: path for the second side's JSON report (default: --out with a `-b` suffix before the extension, e.g. synthetic-report-b.json)."
        )]
        paired_out: Option<String>,
        #[arg(
            long = "paired-label",
            requires = "paired_endpoint",
            help = "with --paired-endpoint: display name for the second side's run (like --label for the first), recorded into its report."
        )]
        paired_label: Option<String>,
    },
    #[command(about = "list the available operations")]
    ListOps,
    #[command(
        about = "record a workload bundle OFFLINE (no server): the dataset load-script + measured commands, so the exact same graph and commands can be replayed across FalkorDB versions"
    )]
    Record {
        #[arg(
            long = "config",
            help = "path to a synthetic-bench.toml config (auto-detected in the CWD if present); CLI flags override it"
        )]
        config: Option<String>,
        #[arg(long, help = "graph key the recorded commands target (default falkor)")]
        graph: Option<String>,
        #[arg(
            long = "op",
            value_parser = OpSelectorValueParser::reads_only(),
            value_delimiter = ',',
            num_args = 1..,
            help = "read operation(s) to record; repeatable and comma-separated. Use --op all (or --op '*') for every read op. Overrides the config's operations."
        )]
        ops: Vec<OpSelector>,
        #[arg(
            long,
            conflicts_with = "ops",
            help = "record every read operation (same as --op all; mutually exclusive with --op)"
        )]
        all_reads: bool,
        #[arg(
            long,
            value_enum,
            conflicts_with_all = ["ops", "all_reads"],
            help = "record a coverage tier: `core` (small per-PR read subset) or `full` (every read op; same as --all-reads). Mutually exclusive with --op/--all-reads."
        )]
        tier: Option<Tier>,
        #[arg(
            long = "repo-reads",
            value_enum,
            conflicts_with_all = ["ops", "all_reads", "tier"],
            help = "record the A/B benchmark's NON-ALGORITHM READ shapes from queries_repository at a coverage tier: `core` (small per-PR subset) or `full` (all 50 reads — 46 baseline + the ExtendedCore temporal/spatial roundtrip + 3 FixtureDependent fulltext/vector reads, whose top-k results are N/A). Auto-discovered + annotated; deterministic (record-once/replay-verbatim). Mutually exclusive with --op/--all-reads/--tier."
        )]
        repo_reads: Option<Tier>,
        #[arg(
            long = "repo-algorithms",
            conflicts_with_all = ["ops", "all_reads", "tier"],
            help = "additionally record the 4 opt-in whole-graph algorithm read shapes (algo.pageRank / algo.maxFlow / algo.MSF / algo.HarmonicCentrality) — algo.maxFlow/algo.MSF result-gated (byte-stable digests), algo.pageRank/algo.HarmonicCentrality result-N/A (arbitrary/iterative floats, design §6); each with a tight per-op budget (C=1, cached, 25 samples) and a small corpus. Orthogonal to --repo-reads (combinable with it or usable alone); never part of --repo-reads or the per-PR gate. Mutually exclusive with --op/--all-reads/--tier."
        )]
        repo_algorithms: bool,
        #[arg(
            long = "repo-writes",
            conflicts_with_all = ["ops", "all_reads", "tier", "repo_reads", "repo_algorithms"],
            help = "record the A/B benchmark's 10 WRITE shapes from queries_repository (CREATE/SET/MERGE/DELETE/REMOVE/FOREACH) as a write bundle (recording format v2; the workload_hash binds each op's read/write kind). The recorded graph includes a deterministic prepared-state statement (every User gains rpc_social_credit + :TemporaryLabel) so the REMOVE shape mutates state that exists. Replay measures them via GRAPH.QUERY at C=1 only, resetting the base graph before every measured cell and restoring + content-verifying it afterwards; results/counters are NOT asserted (latency tier) unless the bundle carries the --oracle outcomes. Write bundles are single-kind: mutually exclusive with every read selector."
        )]
        repo_writes: bool,
        #[arg(
            long,
            help = "seed for the dataset and the per-operation corpora (same seed + same tool build ⇒ identical bundle; default 0)"
        )]
        seed: Option<u64>,
        #[arg(long, help = "dataset node count")]
        nodes: Option<usize>,
        #[arg(long, help = "dataset edge count, must be >= nodes")]
        edges: Option<usize>,
        #[arg(
            long,
            requires = "repo_writes",
            value_name = "ENDPOINT",
            help = "capture the write outcome ORACLE while recording (write bundles only): run EVERY command of each oracle-eligible write shape (9 of the 10 shapes — only the server-rand() single_edge_update is excluded; complete corpus) once against the recorded pristine base on this live FalkorDB endpoint (falkor://host:port), per-invocation restore, capture its mutation counters, prove determinism with a second full pass, and fold the outcomes into the bundle (format v4; hash-bound; the oracle must cover every eligible op exactly — no subset. Format v3 is the frozen seven-op pre-prepared-state layout; v3 bundles still load and replay under their own exact-set rule). Replay then re-verifies every recorded outcome at C=1 before measuring latency; any divergence is a hard replay error naming the op/seq/command."
        )]
        oracle: Option<String>,
        #[arg(
            long = "out-dir",
            help = "directory to write the recording bundle into (manifest.json + graph.jsonl + commands/)"
        )]
        out_dir: String,
    },
    #[command(
        about = "render a saved synthetic report, or DIFF two of them: `report <run.json>` prints the console summary (and writes Markdown only when --out is given); `report --diff <A.json> <B.json>` guards (workload_hash + result digests) then writes a Markdown diff across every op/cache-mode/concurrency"
    )]
    Report {
        #[arg(help = "a saved synthetic report JSON to re-render (prints the console summary; writes Markdown only if --out is set)")]
        input: Option<String>,
        #[arg(
            long = "diff",
            num_args = 2,
            value_names = ["A_JSON", "B_JSON"],
            conflicts_with = "input",
            help = "diff two saved reports A and B (guards that they measured the same workload, then writes the diff)"
        )]
        diff: Vec<String>,
        #[arg(
            long,
            requires = "diff",
            help = "with --diff: emit a NON-FATAL, colored regression report (per-cell 🟢/🔴/N/A by p50 budget; diverged ops are marked, never aborted) instead of the strict diff. Candidate is the second (B) report."
        )]
        regression: bool,
        #[arg(
            long,
            value_name = "FILE",
            requires = "regression",
            help = "TOML thresholds file for --regression (default: built-in 10% budget, 0.5ms floor; per-op + per-op×concurrency overrides)"
        )]
        thresholds: Option<String>,
        #[arg(
            long,
            help = "Markdown output path: the diff (default synthetic-diff.md) with --diff, the regression report (default synthetic-regression.md) with --diff --regression, or the re-rendered report's Markdown when re-rendering a single report"
        )]
        out: Option<String>,
        #[arg(
            long = "elapsed-secs",
            value_name = "SECONDS",
            value_parser = parse_elapsed_secs,
            requires = "regression",
            help = "with --diff --regression: total wall-clock seconds the caller spent computing this check (benchmark + reporting), rendered as a compute-time line in the report header"
        )]
        elapsed_secs: Option<f64>,
        #[arg(
            long = "summary",
            value_name = "FILE",
            requires = "regression",
            help = "with --diff --regression: also write a compact machine-usable SUMMARY (JSON: overall verdict + per-tier 🟢/🔴/⚠/N-A counts + worst offenders + a stable slug) to FILE — small enough for a PR comment, while the full report is hosted externally and linked by the slug"
        )]
        summary: Option<String>,
        #[arg(
            long = "cells",
            value_name = "FILE",
            requires = "regression",
            help = "with --diff --regression: also write the FULL analysis model (JSON: per-op × cache-mode × concurrency cells with medians, deltas, budgets and verdicts, plus the meta block) to FILE — source material for the interactive report page"
        )]
        cells: Option<String>,
        #[arg(
            long = "budget-profile",
            value_name = "PROFILE",
            value_parser = ["strict", "cross-engine"],
            requires = "regression",
            help = "with --diff --regression: which budget profile of the thresholds TOML to apply (default: strict, today's same-engine budgets). `cross-engine` selects the [cross-engine] sections and errors if the TOML doesn't define them"
        )]
        budget_profile: Option<String>,
        #[arg(
            long = "divergence-policy",
            value_name = "POLICY",
            value_parser = ["gate", "advisory"],
            requires = "regression",
            help = "with --diff --regression: how a result divergence affects the verdict. `gate` (default): diverged op is 🔴 and fails the comparison. `advisory`: diverged op is ⚠, perf cells stay N/A, overall verdict caps at advisory — for cross-engine runs where engines legitimately differ"
        )]
        divergence_policy: Option<String>,
    },
}

fn parse_write_ratio(val: &str) -> Result<f32, String> {
    match val.parse::<f32>() {
        Ok(value) if (0.0..=1.0).contains(&value) => Ok(value),
        Ok(_) => Err(String::from("Value must be between 0.0 and 1.0")),
        Err(_) => Err(String::from("Invalid float value")),
    }
}

/// Parse `--elapsed-secs`: a finite, non-negative number of seconds (rejects `-1`, `inf`, `NaN`).
fn parse_elapsed_secs(val: &str) -> Result<f64, String> {
    match val.parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(value),
        Ok(_) => Err(String::from("must be a finite, non-negative number of seconds")),
        Err(_) => Err(String::from("Invalid float value")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_elapsed_secs_accepts_nonnegative_rejects_bad() {
        assert_eq!(parse_elapsed_secs("0").unwrap(), 0.0);
        assert_eq!(parse_elapsed_secs("754.5").unwrap(), 754.5);
        assert!(parse_elapsed_secs("-1").is_err());
        assert!(parse_elapsed_secs("inf").is_err());
        assert!(parse_elapsed_secs("NaN").is_err());
        assert!(parse_elapsed_secs("abc").is_err());
    }

    #[test]
    fn parse_op_selector_accepts_magic_and_names() {
        assert_eq!(parse_op_selector("all").unwrap(), OpSelector::All);
        assert_eq!(parse_op_selector("*").unwrap(), OpSelector::All);
        assert_eq!(
            parse_op_selector("match_by_index").unwrap(),
            OpSelector::One(OpName::MatchByIndex)
        );
        assert!(parse_op_selector("nope").is_err());
    }

    #[test]
    fn expand_op_selectors_merges_dedups_and_keeps_explicit_ops() {
        // `all` alone expands to every read op in canonical order.
        assert_eq!(expand_op_selectors(&[OpSelector::All]), OpName::all_reads());
        // A read op already covered by `all` is not duplicated.
        assert_eq!(
            expand_op_selectors(&[OpSelector::All, OpSelector::One(OpName::MatchByIndex)]),
            OpName::all_reads()
        );
        // A write op listed alongside `all` is kept (not silently dropped) — appended after reads.
        let mut expected = OpName::all_reads();
        expected.push(OpName::CreateNode);
        assert_eq!(
            expand_op_selectors(&[OpSelector::All, OpSelector::One(OpName::CreateNode)]),
            expected
        );
        // Named ops preserved in first-occurrence order, with duplicates removed.
        assert_eq!(
            expand_op_selectors(&[
                OpSelector::One(OpName::Expand1Hop),
                OpSelector::One(OpName::MatchByIndex),
                OpSelector::One(OpName::Expand1Hop),
            ]),
            vec![OpName::Expand1Hop, OpName::MatchByIndex]
        );
        // Empty stays empty (no --op given).
        assert!(expand_op_selectors(&[]).is_empty());
    }

    #[test]
    fn op_selector_value_parser_parses_and_advertises_possible_values() {
        use clap::builder::TypedValueParser;
        let cmd = clap::Command::new("test");
        let parser = OpSelectorValueParser::all_ops();
        // Magic + named values parse via the TypedValueParser (the path clap actually uses).
        assert_eq!(
            parser.parse_ref(&cmd, None, std::ffi::OsStr::new("all")).unwrap(),
            OpSelector::All
        );
        assert_eq!(
            parser.parse_ref(&cmd, None, std::ffi::OsStr::new("*")).unwrap(),
            OpSelector::All
        );
        assert_eq!(
            parser
                .parse_ref(&cmd, None, std::ffi::OsStr::new("match_by_index"))
                .unwrap(),
            OpSelector::One(OpName::MatchByIndex)
        );
        // all_ops advertises every op tag (reads + writes) plus the two magic tokens.
        let possible: Vec<String> = parser
            .possible_values()
            .unwrap()
            .map(|v| v.get_name().to_string())
            .collect();
        assert_eq!(possible.len(), OpName::all().len() + 2);
        assert!(possible.contains(&"match_by_index".to_string()));
        assert!(possible.contains(&"create_node".to_string()));
        assert!(possible.contains(&"all".to_string()));
        assert!(possible.contains(&"*".to_string()));
    }

    #[test]
    fn reads_only_op_parser_excludes_and_rejects_write_ops() {
        use clap::builder::TypedValueParser;
        let cmd = clap::Command::new("test");
        let parser = OpSelectorValueParser::reads_only();
        // A read op and the magic tokens still parse.
        assert_eq!(
            parser
                .parse_ref(&cmd, None, std::ffi::OsStr::new("match_by_index"))
                .unwrap(),
            OpSelector::One(OpName::MatchByIndex)
        );
        assert_eq!(parser.parse_ref(&cmd, None, std::ffi::OsStr::new("all")).unwrap(), OpSelector::All);
        // A write op is rejected at parse time (recording can't record writes).
        assert!(parser
            .parse_ref(&cmd, None, std::ffi::OsStr::new("create_node"))
            .is_err());
        // Possible values are exactly the read ops + all / * (no write tags).
        let possible: Vec<String> = parser
            .possible_values()
            .unwrap()
            .map(|v| v.get_name().to_string())
            .collect();
        assert_eq!(possible.len(), OpName::all_reads().len() + 2);
        assert!(possible.contains(&"match_by_index".to_string()));
        assert!(!possible.contains(&"create_node".to_string()));
    }

    #[test]
    fn cli_op_flag_accepts_magic_and_rejects_unknown() {
        use clap::Parser;
        // `--op all` + comma lists parse end-to-end through the real command tree.
        assert!(Cli::try_parse_from(["benchmark", "synthetic", "run", "--op", "all"]).is_ok());
        assert!(Cli::try_parse_from([
            "benchmark",
            "synthetic",
            "run",
            "--op",
            "match_by_index,expand_1_hop",
        ])
        .is_ok());
        // An unknown op is rejected with a clap error (exercises the arg-context error path).
        assert!(Cli::try_parse_from(["benchmark", "synthetic", "run", "--op", "bogus"]).is_err());
        // `run` accepts write ops, but `record` rejects them (reads-only bundle).
        assert!(Cli::try_parse_from(["benchmark", "synthetic", "run", "--op", "create_node"]).is_ok());
        assert!(Cli::try_parse_from([
            "benchmark",
            "synthetic",
            "record",
            "--op",
            "create_node",
            "--out-dir",
            "/tmp/does-not-matter",
        ])
        .is_err());
    }

    #[test]
    fn cli_tier_flag_parses_and_conflicts_with_op_selection() {
        use clap::Parser;
        // `--tier core|full` parses on both `run` and `record`.
        assert!(Cli::try_parse_from(["benchmark", "synthetic", "run", "--tier", "core"]).is_ok());
        assert!(Cli::try_parse_from(["benchmark", "synthetic", "run", "--tier", "full"]).is_ok());
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "record", "--tier", "core", "--out-dir", "rec-out",
        ])
        .is_ok());
        // An unknown tier is rejected.
        assert!(Cli::try_parse_from(["benchmark", "synthetic", "run", "--tier", "nope"]).is_err());
        // `--tier` is mutually exclusive with `--op` and with `--all-reads`.
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "run", "--tier", "core", "--op", "match_by_index",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "run", "--tier", "core", "--all-reads",
        ])
        .is_err());
    }

    #[test]
    fn cli_repo_reads_flag_parses_and_conflicts_with_op_selection() {
        use clap::Parser;
        // `--repo-reads core|full` parses on `record`.
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "record", "--repo-reads", "core", "--out-dir", "rec-out",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "record", "--repo-reads", "full", "--out-dir", "rec-out",
        ])
        .is_ok());
        // An unknown tier is rejected.
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "record", "--repo-reads", "nope", "--out-dir", "rec-out",
        ])
        .is_err());
        // `--repo-reads` is mutually exclusive with `--op`, `--all-reads` and `--tier`.
        for conflicting in [
            vec!["--op", "match_by_index"],
            vec!["--all-reads"],
            vec!["--tier", "core"],
        ] {
            let mut argv = vec![
                "benchmark", "synthetic", "record", "--repo-reads", "core", "--out-dir", "rec-out",
            ];
            argv.extend(conflicting);
            assert!(
                Cli::try_parse_from(argv.clone()).is_err(),
                "expected conflict for {argv:?}"
            );
        }
        // `--repo-reads` is record-only (not a `run` flag).
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "run", "--repo-reads", "core",
        ])
        .is_err());
    }

    #[test]
    fn cli_require_oracle_needs_recording() {
        use clap::Parser;
        // `--require-oracle` rides on `--recording` (like `--no-load`)…
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "run", "--recording", "rec", "--require-oracle",
        ])
        .is_ok());
        // …and is rejected without it.
        assert!(Cli::try_parse_from([
            "benchmark", "synthetic", "run", "--require-oracle",
        ])
        .is_err());
    }

    #[test]
    fn cli_paired_flags_need_recording_and_paired_endpoint() {
        use clap::Parser;
        // The full paired flag set rides on `--recording`.
        assert!(Cli::try_parse_from([
            "benchmark",
            "synthetic",
            "run",
            "--recording",
            "rec",
            "--paired-endpoint",
            "falkor://127.0.0.1:6380",
            "--paired-graph",
            "g_b",
            "--paired-out",
            "b.json",
            "--paired-label",
            "pr",
        ])
        .is_ok());
        // `--paired-endpoint` alone is enough (out/label/graph default).
        assert!(Cli::try_parse_from([
            "benchmark",
            "synthetic",
            "run",
            "--recording",
            "rec",
            "--paired-endpoint",
            "falkor://127.0.0.1:6380",
        ])
        .is_ok());
        // …but is rejected without `--recording` (paired measurement replays a bundle).
        assert!(Cli::try_parse_from([
            "benchmark",
            "synthetic",
            "run",
            "--paired-endpoint",
            "falkor://127.0.0.1:6380",
        ])
        .is_err());
        // The secondary knobs ride on `--paired-endpoint`, not on `--recording` alone.
        for lone in [
            vec!["--paired-graph", "g_b"],
            vec!["--paired-out", "b.json"],
            vec!["--paired-label", "pr"],
        ] {
            let mut argv = vec!["benchmark", "synthetic", "run", "--recording", "rec"];
            argv.extend(lone);
            assert!(
                Cli::try_parse_from(argv.clone()).is_err(),
                "expected missing --paired-endpoint for {argv:?}"
            );
        }
    }
}
