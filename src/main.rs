// mod cli;

use askama::Template;
use benchmark::cli::Cli;
use benchmark::cli::Commands;
use benchmark::cli::Commands::GenerateAutoComplete;
use benchmark::compare_template::{CompareRuns, CompareTemplate};
use benchmark::error::BenchmarkError::OtherError;
use benchmark::error::BenchmarkResult;
use benchmark::falkor::{Falkor, Started, Stopped};
use benchmark::metrics_collector::{MachineMetadata, MetricsCollector};
use benchmark::queries_repository::{Queries, QueryType};
use benchmark::scenario::{Size, Spec, Vendor};
use benchmark::utils::{delete_file, file_exists, format_number, write_to_file};
use clap::{Command, CommandFactory, Parser};
use clap_complete::{generate, Generator};
use futures::StreamExt;
use histogram::Histogram;
use std::io;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info, instrument, Instrument};

#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    let mut cmd = Cli::command();
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .init();

    let prometheus_endpoint = benchmark::prometheus_endpoint::PrometheusEndpoint::new();

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
            parallel,
        } => {
            let machine_metadata = MachineMetadata::new();
            info!(
                "Run benchmark vendor: {}, graph-size:{}, queries: {}, machine_metadata: {:?}",
                vendor, size, queries, machine_metadata
            );

            match vendor {
                Vendor::Neo4j => {
                    run_neo4j(size, queries).await?;
                }
                Vendor::Falkor => {
                    run_falkor(size, queries, parallel).await?;
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
        Commands::Compare { file1, file2 } => {
            info!(
                "Compare benchmark {} {}",
                file1.path().display(),
                file2.path().display()
            );
            let collector_1 = MetricsCollector::from_file(file1.path()).await?;
            let collector_2 = MetricsCollector::from_file(file2.path()).await?;
            info!("got both collectors");
            let compare_runs = CompareRuns {
                run_1: collector_1.to_percentile(),
                run_2: collector_2.to_percentile(),
            };
            // let json = serde_json::to_string_pretty(&compare_runs)?;
            // info!("{}", json);
            let compare_template = CompareTemplate { data: compare_runs };
            let compare_report = compare_template.render().unwrap();
            write_to_file("html/compare.html", compare_report.as_str()).await?;
            info!("report was written to html/compare.html");
            // println!("{}", compare_report);
        }
    }
    drop(prometheus_endpoint);
    Ok(())
}

async fn run_neo4j(
    size: Size,
    number_of_queries: u64,
) -> BenchmarkResult<()> {
    let neo4j = benchmark::neo4j::Neo4j::new();
    // stop neo4j if it is running
    neo4j.stop(false).await?;
    // restore the dump
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Neo4j);
    neo4j.restore_db(spec).await?;
    // start neo4j
    neo4j.start().await?;
    let client = neo4j.client().await?;
    info!("client connected to neo4j");
    // get the graph size
    let (node_count, relation_count) = client.graph_size().await?;
    let mut metric_collector = MetricsCollector::new(
        node_count,
        relation_count,
        number_of_queries,
        "Neo4J".to_owned(),
    )?;
    // generate queries
    let mut queries_repository =
        benchmark::queries_repository::UsersQueriesRepository::new(9998, 121716);
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
    metric_collector
        .save(format!(
            "neo4j-metrics_{}_q{}.json",
            size, number_of_queries
        ))
        .await?;
    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries),
        elapsed
    );
    neo4j.stop(true).await?;
    // stop neo4j
    // write the report
    Ok(())
}
#[instrument]
async fn run_falkor(
    size: Size,
    number_of_queries: u64,
    parallel: usize,
) -> BenchmarkResult<()> {
    let falkor: Falkor<Stopped> = benchmark::falkor::Falkor::new();

    // if dump not present return error
    falkor.dump_exists_or_error(size).await?;
    // restore the dump
    falkor.restore_db(size).await?;
    // start falkor
    let mut falkor = falkor.start().await?;

    // get the graph size
    let (node_count, relation_count) = falkor.graph_size().await?;

    info!(
        "graph has {} nodes and {} relations",
        format_number(node_count),
        format_number(relation_count)
    );
    info!("running {} queries", format_number(number_of_queries));

    let mut queries_repository =
        benchmark::queries_repository::UsersQueriesRepository::new(9998, 121716);
    let queries: Vec<(String, QueryType, String)> = queries_repository
        .random_queries(number_of_queries)
        .map(|(q_name, q_type, q)| (q_name, q_type, q.to_cypher()))
        .collect::<Vec<_>>();

    let mut handles = Vec::with_capacity(parallel);
    let start = Instant::now();
    for spawn_id in 0..parallel {
        let queries = queries.clone();
        let handle = spawn_worker(&mut falkor, queries, spawn_id).await?;
        handles.push(handle);
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        // Await each handle to get the result of the task
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => eprintln!("Task failed: {:?}", e),
        }
    }

    let elapsed = start.elapsed();
    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries),
        elapsed
    );
    // stop falkor
    let _stopped = falkor.stop().await?;
    Ok(())
}

#[instrument(skip(falkor, queries), fields(vendor = "falkor",  query_count = queries.len()))]
async fn spawn_worker(
    falkor: &mut Falkor<Started>,
    queries: Vec<(String, QueryType, String)>,
    spawn_id: usize,
) -> BenchmarkResult<JoinHandle<()>> {
    info!("spawning worker");
    let queries = queries.clone();
    let mut graph = falkor.client().await?;
    let handle = tokio::spawn(
        async move {
            graph.execute_queries(spawn_id, queries).await;
        }
        .instrument(tracing::Span::current()),
    );

    Ok(handle)
}

async fn init_falkor(
    size: Size,
    _force: bool,
) -> BenchmarkResult<()> {
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Neo4j);
    let falkor = benchmark::falkor::Falkor::new();
    falkor.clean_db().await?;

    let falkor = falkor.start().await?;
    info!("writing index and data");
    // let index_iterator = spec.init_index_iterator().await?;
    let start = Instant::now();

    let mut falkor_client = falkor.client().await?;
    falkor_client
        .execute_query(
            "main",
            "create_index",
            "CREATE INDEX FOR (u:User) ON (u.id)",
        )
        .await?;

    let mut data_iterator = spec.init_data_iterator().await?;

    while let Some(result) = data_iterator.next().await {
        match result {
            Ok(query) => {
                falkor_client
                    .execute_query("loader", "", query.as_str())
                    .await?;
            }
            Err(e) => {
                error!("error {}", e);
            }
        }
    }

    let (node_count, relation_count) = falkor.graph_size().await?;
    info!(
        "{} nodes and {} relations were imported at {:?}",
        format_number(node_count),
        format_number(relation_count),
        start.elapsed()
    );
    info!("writing done, took: {:?}", start.elapsed());
    let falkor = falkor.stop().await?;
    falkor.save_db(size).await?;

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
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Neo4j);
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
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Neo4j);
    let neo4j = benchmark::neo4j::Neo4j::new();
    let _ = neo4j.stop(false).await?;
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
    neo4j.clone().stop(true).await?;
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