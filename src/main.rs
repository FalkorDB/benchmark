mod cli;
mod error;
mod falkor;
mod metrics_collector;
mod neo4j;
mod neo4j_client;
mod neo4j_process;
mod queries_repository;
mod query;
pub mod scenario;
mod utils;

use crate::cli::Commands;
use crate::cli::Commands::GenerateAutoComplete;
use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::{Connected, Disconnected, Falkor};
use crate::metrics_collector::MetricsCollector;
use crate::queries_repository::Queries;
use crate::scenario::{Size, Spec, Vendor};
use crate::utils::{delete_file, file_exists, format_number, write_to_file};
use clap::{Command, CommandFactory, Parser};
use clap_complete::{generate, Generator};
use cli::Cli;
use futures::StreamExt;
use histogram::Histogram;
use std::io;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(Level::INFO)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    let mut cmd = Cli::command();
    let cli = Cli::parse();
    match cli.command {
        GenerateAutoComplete { shell } => {
            eprintln!("Generating completion file for {shell}...");
            print_completions(shell, &mut cmd);
        }

        Commands::Init {
            vendor,
            size,
            force,
            dry_run,
        } => {
            info!("Init benchmark {} {} {}", vendor, size, force);
            match vendor {
                Vendor::Neo4j => {
                    if dry_run {
                        dry_init_neo4j(size).await?;
                    } else {
                        init_neo4j(size, force).await?;
                    }
                }
                Vendor::Falkor => {
                    if dry_run {
                        info!("Dry run");
                        todo!()
                    } else {
                        init_falkor(size, force).await?;
                    }
                }
            }
        }
        Commands::Run {
            vendor,
            size,
            queries,
        } => {
            info!(
                "Run benchmark vendor: {}, graph-size:{}, queries: {}",
                vendor, size, queries
            );
            match vendor {
                Vendor::Neo4j => {
                    run_neo4j(size, queries).await?;
                }
                Vendor::Falkor => {
                    run_falkor(size, queries).await?;
                }
            }
        }

        Commands::Clear {
            vendor,
            size,
            force,
        } => {
            info!("Clear benchmark {} {} {}", vendor, size, force);
        }
    }

    Ok(())
}

async fn run_neo4j(
    size: Size,
    number_of_queries: u64,
) -> BenchmarkResult<()> {
    let neo4j = neo4j::Neo4j::new();
    // stop neo4j if it is running
    neo4j.stop().await?;
    // restore the dump
    let spec = Spec::new(scenario::Name::Users, size, Vendor::Neo4j);
    neo4j.restore_db(spec).await?;
    // start neo4j
    neo4j.start().await?;
    let client = neo4j.client().await?;
    info!("client connected to neo4j");
    // get the graph size
    let (node_count, relation_count) = client.graph_size().await?;
    let mut metric_collector =
        MetricsCollector::new(node_count, relation_count, number_of_queries, "Neo4J")?;
    // generate queries
    let queries_repository = queries_repository::UsersQueriesRepository::new(9998, 121716);
    let queries = Box::new(
        queries_repository
            .random_queries(number_of_queries)
            .map(|(q_name, q_type, q)| (q_name, q_type, q.to_bolt())),
    );
    info!(
        "graph has {} nodes and {} relations",
        format_number(node_count),
        format_number(relation_count)
    );
    info!("running {} queries", format_number(number_of_queries));
    let start = Instant::now();
    client
        .clone()
        .execute_query_iterator(queries, &mut metric_collector)
        .await?;
    let elapsed = start.elapsed();
    let report_file = "neo4j-results.md";
    info!("report was written to {}", report_file);
    write_to_file(report_file, metric_collector.markdown_report().as_str()).await?;
    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries),
        elapsed
    );
    neo4j.stop().await?;
    // stop neo4j
    // write the report
    Ok(())
}
async fn run_falkor(
    size: Size,
    number_of_queries: u64,
) -> BenchmarkResult<()> {
    let falkor: Falkor<Disconnected> = falkor::Falkor::new();
    // stop falkor if it is running
    falkor.stop(false).await?;
    // if dump not present return error
    falkor.dump_exists_or_error(size).await?;
    // restore the dump
    falkor.restore_db(size).await?;
    // start falkor
    falkor.start().await?;
    // connect to falkor
    let mut falkor: Falkor<Connected> = falkor.connect().await?;
    // get the graph size
    let (node_count, relation_count) = falkor.graph_size().await?;
    let mut metric_collector =
        MetricsCollector::new(node_count, relation_count, number_of_queries, "FalkorDB")?;
    // generate queries
    let queries_repository = queries_repository::UsersQueriesRepository::new(9998, 121716);
    let queries = Box::new(
        queries_repository
            .random_queries(number_of_queries)
            .map(|(q_name, q_type, q)| (q_name, q_type, q.to_cypher())),
    );
    info!(
        "graph has {} nodes and {} relations",
        format_number(node_count),
        format_number(relation_count)
    );
    info!("running {} queries", format_number(number_of_queries));
    let start = Instant::now();
    falkor
        .execute_query_iterator(queries, &mut metric_collector)
        .await?;
    let elapsed = start.elapsed();
    let report_file = "falkor-results.md";
    info!("report was written to {}", report_file);
    write_to_file(report_file, metric_collector.markdown_report().as_str()).await?;
    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries),
        elapsed
    );
    // stop falkor
    falkor.stop(false).await
}

async fn init_falkor(
    size: Size,
    _force: bool,
) -> BenchmarkResult<()> {
    let mut histogram = Histogram::new(7, 64)?;
    let spec = Spec::new(scenario::Name::Users, size, Vendor::Neo4j);
    let falkor = falkor::Falkor::new();

    falkor.clean_db().await?;
    // falkor.restore_db(size).await?;

    falkor.start().await?;
    info!("writing index and data");
    // let index_iterator = spec.init_index_iterator().await?;
    let mut falkor = falkor.connect().await?;
    let start = Instant::now();
    falkor
        .execute_query("CREATE INDEX FOR (u:User) ON (u.id);")
        .await?;
    let data_iterator = spec.init_data_iterator().await?;
    falkor
        .execute_query_stream(data_iterator, &mut histogram)
        .await?;
    let (node_count, relation_count) = falkor.graph_size().await?;
    info!(
        "{} nodes and {} relations were imported at {:?}",
        format_number(node_count),
        format_number(relation_count),
        start.elapsed()
    );
    info!("writing done, took: {:?}", start.elapsed());
    falkor.disconnect().await?;
    falkor.stop(true).await?;
    falkor.save_db(size).await?;

    show_historgam(histogram);
    Ok(())
}

fn show_historgam(histogram: Histogram) {
    for percentile in 1..=99 {
        let p = histogram
            .percentile(percentile as f64)
            .map(|r| r.map(|b| Duration::from_micros(b.end())));

        info!("p{}: {:?}", percentile, p);
    }
}

async fn dry_init_neo4j(size: Size) -> BenchmarkResult<()> {
    let spec = Spec::new(scenario::Name::Users, size, Vendor::Neo4j);
    let mut data_stream = spec.init_data_iterator().await?;
    let mut success = 0;
    let mut error = 0;

    let start = Instant::now();
    while let Some(result) = data_stream.next().await {
        match result {
            Ok(_query) => {
                success += 1;
            }
            Err(e) => {
                error!("error {}", e);
                error += 1;
            }
        }
    }
    info!(
        "importing (dry run) done at {:?}, {} records process successfully, {} failed",
        start.elapsed(),
        success,
        error
    );
    Ok(())
}
async fn init_neo4j(
    size: Size,
    force: bool,
) -> BenchmarkResult<()> {
    let spec = Spec::new(scenario::Name::Users, size, Vendor::Neo4j);
    let neo4j = neo4j::Neo4j::new();
    let _ = neo4j.stop().await?;
    let backup_path = format!("{}/neo4j.dump", spec.backup_path());
    if !force {
        if file_exists(backup_path.as_str()).await && !force {
            info!(
                "Backup file exists, skipping init, use --force to override ({})",
                backup_path.as_str()
            );
            return Ok(());
        }
    } else {
        delete_file(backup_path.as_str()).await?;
        let out = neo4j.clean_db().await?;
        info!(
            "neo clean_db std_error returns {} ",
            String::from_utf8_lossy(&out.stderr)
        );
        info!(
            "neo clean_db std_out returns {} ",
            String::from_utf8_lossy(&out.stdout)
        );
        // @ todo delete the data and index file as well
        // delete_file(spec.cache(spec.data_url.as_ref()).await?.as_str()).await;
    }

    neo4j.start().await?;

    let client = neo4j.client().await?;
    let (node_count, relation_count) = client.graph_size().await?;
    info!(
        "node count: {}, relation count: {}",
        format_number(node_count),
        format_number(relation_count)
    );
    if node_count != 0 || relation_count != 0 {
        error!(
            "graph is not empty, node count: {}, relation count: {}",
            node_count, relation_count
        );
        return Err(OtherError("graph is not empty".to_string()));
    }
    let mut histogram = Histogram::new(7, 64)?;

    let mut index_stream = spec.init_index_iterator().await?;
    info!("importing indexes");
    client
        .execute_query_stream(&mut index_stream, &mut histogram)
        .await?;
    let mut data_stream = spec.init_data_iterator().await?;
    info!("importing data");
    let start = Instant::now();
    client
        .execute_query_stream(&mut data_stream, &mut histogram)
        .await?;
    let (node_count, relation_count) = client.graph_size().await?;
    info!(
        "{} nodes and {} relations were imported at {:?}",
        format_number(node_count),
        format_number(relation_count),
        start.elapsed()
    );
    neo4j.clone().stop().await?;
    neo4j.dump(spec.clone()).await?;
    info!("---> histogram");

    show_historgam(histogram);

    info!("---> Done");
    Ok(())
}

fn print_completions<G: Generator>(
    gen: G,
    cmd: &mut Command,
) {
    generate(gen, cmd, cmd.get_name().to_string(), &mut io::stdout());
}
