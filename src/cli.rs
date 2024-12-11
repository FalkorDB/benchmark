use crate::scenario::Vendor;
use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser, Debug)]
#[command(name = "benchmark", version, about="falkor benchmark tool", long_about = None, arg_required_else_help(true), propagate_version(true))]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(arg_required_else_help = true)]
    GenerateAutoComplete { shell: Shell },
    #[command(arg_required_else_help = true)]
    Init {
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
    },
    Clear {
        #[arg(short, long, value_enum)]
        vendor: Vendor,
        #[arg(short, long, value_enum)]
        size: crate::scenario::Size,
        #[arg(
            short,
            long,
            value_enum,
            default_value = "false",
            help = "Clear cache and backups as well as the database"
        )]
        force: bool,
    },

    PrepareQueries {
        #[arg(short, long, value_enum)]
        dataset_size: crate::scenario::Size,
        #[arg(short = 'q', long, alias = "queries", default_value_t = 1000000)]
        number_of_queries: u64,
        #[arg(
            short = 'w',
            long,
            aliases = ["workers", "parallel"],
            default_value_t = 1
        )]
        number_of_workers: usize,
        #[arg(short = 'n', long, help = "the name of this query set")]
        name: String,
    },
    Run {
        #[arg(short, long, value_enum)]
        vendor: Vendor,
        #[arg(short, long, value_enum)]
        size: crate::scenario::Size,
        #[arg(
            short,
            long,
            required = false,
            default_value_t = 10000,
            default_missing_value = "10000",
            help = "Number of queries in this benchmark run"
        )]
        queries: u64,
        #[arg(
            short,
            long,
            required = false,
            default_value_t = 1,
            default_missing_value = "1",
            help = "parallelism level"
        )]
        parallel: usize,
    },
}
