//! `synthetic-bench.toml` config file and the merge that turns it + CLI flags into a [`Config`].
//!
//! The growing knob set lives in an optional TOML file; any value can be overridden by an explicit
//! CLI flag (precedence: **CLI flag > file value > built-in default**). Dataset *generation* is
//! never authorized by the file alone — it drops and rewrites the target graph, so it requires the
//! explicit `--generate` CLI flag (the file only supplies the dimensions).

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::synthetic::dataset::DatasetSpec;
use crate::synthetic::{CacheSelection, Config, OpName, Tier, DEFAULT_GRAPH};
use serde::Deserialize;
use std::path::Path;

/// The default config file auto-detected in the working directory when `--config` isn't given.
pub const DEFAULT_CONFIG_FILE: &str = "synthetic-bench.toml";

/// Parsed `synthetic-bench.toml`. Every field is optional; unknown keys are rejected so a typo
/// (e.g. `node = 1000`) fails loudly instead of being silently ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub endpoint: Option<String>,
    pub graph: Option<String>,
    pub operations: Option<Vec<OpName>>,
    pub samples: Option<usize>,
    pub warmup: Option<usize>,
    /// Concurrency sweep (closed-loop worker counts). Defaults to the built-in sweep when omitted.
    pub concurrency: Option<Vec<usize>>,
    /// Reset cadence for write ops (untimed scratch reset every N ops). Ignored by read ops.
    pub reset_every: Option<usize>,
    pub seed: Option<u64>,
    pub cache: Option<CacheSelection>,
    pub server_timeout_ms: Option<i64>,
    pub client_deadline_ms: Option<u64>,
    pub out: Option<String>,
    /// Dataset dimensions (used only when generation is requested via `--generate`).
    pub nodes: Option<usize>,
    pub edges: Option<usize>,
}

impl FileConfig {
    /// Parse a `FileConfig` from TOML text.
    pub fn from_toml(text: &str) -> BenchmarkResult<FileConfig> {
        toml::from_str(text).map_err(|e| OtherError(format!("invalid synthetic config: {}", e)))
    }

    /// Load the config from `path`, or return `None` if `path` is `None` and no default file exists.
    /// An explicitly-requested path that is missing/invalid is an error.
    pub fn load(path: Option<&str>) -> BenchmarkResult<Option<FileConfig>> {
        match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| OtherError(format!("could not read config '{}': {}", p, e)))?;
                Ok(Some(FileConfig::from_toml(&text)?))
            }
            None => {
                if Path::new(DEFAULT_CONFIG_FILE).exists() {
                    let text = std::fs::read_to_string(DEFAULT_CONFIG_FILE).map_err(|e| {
                        OtherError(format!("could not read {}: {}", DEFAULT_CONFIG_FILE, e))
                    })?;
                    Ok(Some(FileConfig::from_toml(&text)?))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

/// The subset of CLI flags that can override the file/defaults. `None` means "flag not passed".
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub endpoint: Option<String>,
    pub graph: Option<String>,
    /// Explicit `--op` list (empty = not passed).
    pub ops: Vec<OpName>,
    /// `--all-reads` selects every read op (mutually exclusive with `--op`).
    pub all_reads: bool,
    /// `--tier <core|full>` selects the read ops in that tier (mutually exclusive with
    /// `--op`/`--all-reads`). `None` = not passed.
    pub tier: Option<Tier>,
    pub samples: Option<usize>,
    pub warmup: Option<usize>,
    /// Explicit `--concurrency` sweep (empty = not passed).
    pub concurrency: Vec<usize>,
    /// `--reset-every` write reset cadence (`None` = not passed).
    pub reset_every: Option<usize>,
    pub seed: Option<u64>,
    pub cache: Option<CacheSelection>,
    pub server_timeout_ms: Option<i64>,
    pub client_deadline_ms: Option<u64>,
    pub out: Option<String>,
    pub server_image: Option<String>,
    pub label: Option<String>,
    /// Explicit destructive consent to generate + load a dataset into `graph`.
    pub generate: bool,
    pub nodes: Option<usize>,
    pub edges: Option<usize>,
}

/// Merge CLI overrides over an optional file config to produce the final [`Config`].
///
/// Operation selection precedence: explicit `--op` replaces everything; else `--tier <t>` (the
/// read ops in that tier); else `--all-reads`; else the file's `operations`; else it's an error
/// (nothing to measure). Generation is enabled only by `--generate`, and then `nodes`/`edges` must
/// resolve from a flag or the file.
pub fn resolve(
    cli: CliOverrides,
    file: Option<FileConfig>,
) -> BenchmarkResult<Config> {
    let file = file.unwrap_or_default();
    let defaults = Config::default();

    let ops = if !cli.ops.is_empty() {
        cli.ops.clone()
    } else if let Some(tier) = cli.tier {
        OpName::reads_in_tier(tier)
    } else if cli.all_reads {
        OpName::all_reads()
    } else {
        file.operations.clone().unwrap_or_default()
    };
    if ops.is_empty() {
        return Err(OtherError(
            "no operations selected — pass --op <name> (repeatable/comma-separated), --all-reads, \
             --tier <core|full>, or set `operations = [...]` in the config file"
                .to_string(),
        ));
    }

    let seed = cli.seed.or(file.seed).unwrap_or(defaults.seed);

    // Concurrency sweep: explicit `--concurrency` wins, else the file's, else the built-in sweep.
    // Validation (non-empty, ≥1, dedup/sort) happens in `run()` via `normalize_concurrency`.
    let concurrency = if !cli.concurrency.is_empty() {
        cli.concurrency.clone()
    } else {
        file.concurrency
            .clone()
            .unwrap_or_else(|| defaults.concurrency.clone())
    };

    let dataset = if cli.generate {
        let nodes = cli.nodes.or(file.nodes).ok_or_else(|| {
            OtherError("--generate needs --nodes (or `nodes` in the config)".to_string())
        })?;
        let edges = cli.edges.or(file.edges).ok_or_else(|| {
            OtherError("--generate needs --edges (or `edges` in the config)".to_string())
        })?;
        let spec = DatasetSpec { seed, nodes, edges };
        spec.validate()?;
        Some(spec)
    } else {
        None
    };

    // Destructure the defaults once so each field is a plain local (no partial moves out of a
    // struct that's still in use below).
    let Config {
        endpoint: default_endpoint,
        samples: default_samples,
        warmup: default_warmup,
        reset_every: default_reset_every,
        server_timeout_ms: default_server_timeout_ms,
        client_deadline_ms: default_client_deadline_ms,
        cache: default_cache,
        out: default_out,
        ..
    } = defaults;

    Ok(Config {
        endpoint: cli.endpoint.or(file.endpoint).unwrap_or(default_endpoint),
        graph: cli
            .graph
            .or(file.graph)
            .unwrap_or_else(|| DEFAULT_GRAPH.to_string()),
        ops,
        samples: cli.samples.or(file.samples).unwrap_or(default_samples),
        warmup: cli.warmup.or(file.warmup).unwrap_or(default_warmup),
        concurrency,
        reset_every: cli
            .reset_every
            .or(file.reset_every)
            .unwrap_or(default_reset_every),
        seed,
        server_timeout_ms: cli
            .server_timeout_ms
            .or(file.server_timeout_ms)
            .unwrap_or(default_server_timeout_ms),
        client_deadline_ms: cli
            .client_deadline_ms
            .or(file.client_deadline_ms)
            .unwrap_or(default_client_deadline_ms),
        cache: cli.cache.or(file.cache).unwrap_or(default_cache),
        out: cli.out.or(file.out).unwrap_or(default_out),
        server_image: cli.server_image,
        label: cli.label,
        dataset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_operations_by_cli_name() {
        let cfg = FileConfig::from_toml(
            "seed = 42\nnodes = 100\nedges = 500\noperations = [\"match_by_index\", \"expand_1_hop\"]\n",
        )
        .unwrap();
        assert_eq!(cfg.seed, Some(42));
        assert_eq!(cfg.nodes, Some(100));
        assert_eq!(
            cfg.operations.unwrap(),
            vec![OpName::MatchByIndex, OpName::Expand1Hop]
        );
    }

    #[test]
    fn rejects_unknown_keys_and_bad_op() {
        assert!(FileConfig::from_toml("node = 100\n").is_err()); // typo: `node`
        assert!(FileConfig::from_toml("operations = [\"nope\"]\n").is_err());
        // digit-name ops use the CLI spelling, not serde's default snake_case.
        assert!(FileConfig::from_toml("operations = [\"expand1_hop\"]\n").is_err());
    }

    #[test]
    fn cli_overrides_file_which_overrides_defaults() {
        let file = FileConfig {
            samples: Some(50),
            graph: Some("from_file".to_string()),
            operations: Some(vec![OpName::MatchByIndex]),
            cache: Some(CacheSelection::Cached),
            concurrency: Some(vec![2, 8]),
            ..Default::default()
        };
        let cli = CliOverrides {
            samples: Some(999),          // overrides file's 50
            concurrency: vec![1, 4, 16], // overrides file's [2, 8]
            ..Default::default()
        };
        let cfg = resolve(cli, Some(file)).unwrap();
        assert_eq!(cfg.samples, 999); // CLI wins
        assert_eq!(cfg.graph, "from_file"); // file wins over default
        assert_eq!(cfg.warmup, Config::default().warmup); // default (unset anywhere)
        assert_eq!(cfg.ops, vec![OpName::MatchByIndex]); // from file
        assert_eq!(cfg.cache, CacheSelection::Cached);
        assert_eq!(cfg.concurrency, vec![1, 4, 16]); // CLI wins over file
    }

    #[test]
    fn concurrency_falls_back_to_file_then_default() {
        // No CLI concurrency ⇒ the file's sweep is used.
        let file = FileConfig {
            operations: Some(vec![OpName::ReturnConst]),
            concurrency: Some(vec![4, 32]),
            ..Default::default()
        };
        let cfg = resolve(CliOverrides::default(), Some(file)).unwrap();
        assert_eq!(cfg.concurrency, vec![4, 32]);

        // Neither CLI nor file ⇒ the built-in default sweep.
        let file2 = FileConfig {
            operations: Some(vec![OpName::ReturnConst]),
            ..Default::default()
        };
        let cfg2 = resolve(CliOverrides::default(), Some(file2)).unwrap();
        assert_eq!(cfg2.concurrency, Config::default().concurrency);
    }

    #[test]
    fn reset_every_precedence_cli_over_file_over_default() {
        // CLI wins over the file.
        let file = FileConfig {
            operations: Some(vec![OpName::CreateNode]),
            reset_every: Some(3),
            ..Default::default()
        };
        let cli = CliOverrides {
            reset_every: Some(7),
            ..Default::default()
        };
        assert_eq!(resolve(cli, Some(file.clone())).unwrap().reset_every, 7);

        // No CLI ⇒ the file's value.
        assert_eq!(
            resolve(CliOverrides::default(), Some(file))
                .unwrap()
                .reset_every,
            3
        );

        // Neither CLI nor file ⇒ the built-in default.
        let bare = FileConfig {
            operations: Some(vec![OpName::CreateNode]),
            ..Default::default()
        };
        assert_eq!(
            resolve(CliOverrides::default(), Some(bare))
                .unwrap()
                .reset_every,
            Config::default().reset_every
        );
    }

    #[test]
    fn reset_every_parses_from_toml() {
        let cfg =
            FileConfig::from_toml("operations = [\"create_node\"]\nreset_every = 12345\n").unwrap();
        assert_eq!(cfg.reset_every, Some(12345));
    }

    #[test]
    fn cli_ops_replace_file_ops() {
        let file = FileConfig {
            operations: Some(vec![OpName::MatchByIndex, OpName::Expand1Hop]),
            ..Default::default()
        };
        let cli = CliOverrides {
            ops: vec![OpName::AggregateCount],
            ..Default::default()
        };
        let cfg = resolve(cli, Some(file)).unwrap();
        assert_eq!(cfg.ops, vec![OpName::AggregateCount]);
    }

    #[test]
    fn all_reads_selects_everything() {
        let cli = CliOverrides {
            all_reads: true,
            ..Default::default()
        };
        let cfg = resolve(cli, None).unwrap();
        assert_eq!(cfg.ops, OpName::all_reads());
    }

    #[test]
    fn tier_selects_that_read_subset_with_op_taking_precedence() {
        // `--tier core` resolves to the core read subset…
        let core = resolve(
            CliOverrides {
                tier: Some(Tier::Core),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(core.ops, OpName::reads_in_tier(Tier::Core));

        // …`--tier full` equals `--all-reads`…
        let full = resolve(
            CliOverrides {
                tier: Some(Tier::Full),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(full.ops, OpName::all_reads());

        // …and `--tier` overrides the file's `operations`, while an explicit `--op` still wins.
        let file = FileConfig {
            operations: Some(vec![OpName::ShortestPath]),
            ..Default::default()
        };
        let over_file = resolve(
            CliOverrides {
                tier: Some(Tier::Core),
                ..Default::default()
            },
            Some(file.clone()),
        )
        .unwrap();
        assert_eq!(over_file.ops, OpName::reads_in_tier(Tier::Core));

        let op_wins = resolve(
            CliOverrides {
                ops: vec![OpName::AggregateGroup],
                tier: Some(Tier::Core),
                ..Default::default()
            },
            Some(file),
        )
        .unwrap();
        assert_eq!(op_wins.ops, vec![OpName::AggregateGroup]);
    }

    #[test]
    fn no_ops_is_an_error() {
        assert!(resolve(CliOverrides::default(), None).is_err());
    }

    #[test]
    fn generate_requires_dimensions_and_is_cli_gated() {
        // File supplies nodes/edges/operations, but without --generate no dataset is built.
        let file = FileConfig {
            nodes: Some(100),
            edges: Some(500),
            operations: Some(vec![OpName::MatchByIndex]),
            ..Default::default()
        };
        let cfg = resolve(CliOverrides::default(), Some(file.clone())).unwrap();
        assert!(
            cfg.dataset.is_none(),
            "file alone must not authorize generation"
        );

        // --generate + file dimensions ⇒ a validated DatasetSpec.
        let cli = CliOverrides {
            generate: true,
            ..Default::default()
        };
        let spec = resolve(cli, Some(file)).unwrap().dataset.unwrap();
        assert_eq!((spec.seed, spec.nodes, spec.edges), (0, 100, 500));

        // --generate without dimensions anywhere ⇒ error (missing nodes).
        let cli = CliOverrides {
            generate: true,
            all_reads: true,
            ..Default::default()
        };
        assert!(resolve(cli, None).is_err());

        // --generate with nodes but no edges ⇒ error (missing edges branch).
        let cli = CliOverrides {
            generate: true,
            nodes: Some(100),
            all_reads: true,
            ..Default::default()
        };
        assert!(resolve(cli, None).is_err());

        // --generate with invalid dimensions (edges < nodes) ⇒ error.
        let cli = CliOverrides {
            generate: true,
            nodes: Some(100),
            edges: Some(10),
            all_reads: true,
            ..Default::default()
        };
        assert!(resolve(cli, None).is_err());
    }

    #[test]
    fn cli_seed_flows_into_dataset_and_corpus() {
        let cli = CliOverrides {
            generate: true,
            nodes: Some(50),
            edges: Some(200),
            seed: Some(7),
            all_reads: true,
            ..Default::default()
        };
        let cfg = resolve(cli, None).unwrap();
        assert_eq!(cfg.seed, 7);
        assert_eq!(cfg.dataset.unwrap().seed, 7);
    }

    #[test]
    fn load_reads_explicit_path_and_errors_on_missing() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);

        // An explicitly-requested but missing path is an error.
        assert!(FileConfig::load(Some("/nonexistent/synthetic-bench.toml")).is_err());

        // A real file loads. The name mixes pid + a process-unique counter so parallel tests can't
        // collide on the same path.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "syn-load-{}-{}.toml",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, "seed = 9\nnodes = 20\nedges = 100\n").unwrap();
        let cfg = FileConfig::load(Some(path.to_str().unwrap()))
            .unwrap()
            .expect("config present");
        assert_eq!(cfg.seed, Some(9));
        assert_eq!(cfg.nodes, Some(20));
        let _ = std::fs::remove_file(&path);
    }
}
