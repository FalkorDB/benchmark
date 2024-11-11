use crate::scenario::Vendor;
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(name = "benchmark", version, about="falkor benchmark tool", long_about = None, arg_required_else_help(true), propagate_version(true))]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
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
    },

    Compare {
        #[arg(required = true)]
        file1: ExistingJsonFile,
        #[arg(required = true)]
        file2: ExistingJsonFile,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ExistingJsonFile(PathBuf);

impl ExistingJsonFile {
    pub fn path(&self) -> &PathBuf {
        &self.0
    }
}
impl FromStr for ExistingJsonFile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = PathBuf::from(s);

        if !path.exists() {
            return Err(format!("File does not exist: {}", s));
        }

        if !(path.extension().and_then(|ext| ext.to_str()) == Some("json")) {
            return Err(format!("File must have a .json extension: {}", s));
        }

        Ok(ExistingJsonFile(path))
    }
}
