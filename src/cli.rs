use crate::queries_repository::{QueryCoverageProfile, QueryType};
use crate::scenario::Vendor;
use crate::synthetic::{CacheSelection, OpName};
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
            help = "measure a RECORDED workload bundle (from `synthetic record`) instead of generating/probing: loads the recorded graph, then measures the recorded commands across --concurrency + --cache. Conflicts with --config/--generate/--op/--all-reads/--nodes/--edges/--seed."
        )]
        recording: Option<String>,
        #[arg(
            long = "no-load",
            requires = "recording",
            help = "with --recording: skip loading the recorded graph, only count-verify the already-loaded graph (load-once / run-many)."
        )]
        no_load: bool,
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
            help = "seed for the dataset and the per-operation corpora (same seed + same tool build ⇒ identical bundle; default 0)"
        )]
        seed: Option<u64>,
        #[arg(long, help = "dataset node count")]
        nodes: Option<usize>,
        #[arg(long, help = "dataset edge count, must be >= nodes")]
        edges: Option<usize>,
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
    },
}

fn parse_write_ratio(val: &str) -> Result<f32, String> {
    match val.parse::<f32>() {
        Ok(value) if (0.0..=1.0).contains(&value) => Ok(value),
        Ok(_) => Err(String::from("Value must be between 0.0 and 1.0")),
        Err(_) => Err(String::from("Invalid float value")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
