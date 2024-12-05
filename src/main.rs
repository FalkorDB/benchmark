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
use benchmark::queries_repository::PreparedQuery;
use benchmark::scenario::{Size, Spec, Vendor};
use benchmark::utils::{delete_file, file_exists, format_number, write_to_file};
use clap::{Command, CommandFactory, Parser};
use clap_complete::{generate, Generator};
use futures::StreamExt;
use histogram::Histogram;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info, instrument};

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
        Commands::PrepareQueries {
            dataset_size,
            number_of_queries,
            number_of_workers,
            name,
        } => {
            info!(
                "Prepare queries dataset_size: {}, number_of_queries: {}, number_of_workers: {}, name: {}",
                dataset_size, number_of_queries, number_of_workers, name
            );
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

    // generate queries
    let queries_repository =
        benchmark::queries_repository::UsersQueriesRepository::new(9998, 121716);
    let queries = Box::new(queries_repository.random_queries(number_of_queries));
    info!(
        "graph has {} nodes and {} relations",
        format_number(node_count),
        format_number(relation_count)
    );
    info!("running {} queries", format_number(number_of_queries));
    let start = Instant::now();
    client.clone().execute_query_iterator(queries).await?;
    let elapsed = start.elapsed();
    let report_file = "neo4j-results.md";
    info!("report was written to {}", report_file);

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
    if parallel == 0 {
        return Err(OtherError(
            "Parallelism level must be greater than zero.".to_string(),
        ));
    }
    let falkor: Falkor<Stopped> = benchmark::falkor::Falkor::new();

    // if dump not present return error
    falkor.dump_exists_or_error(size).await?;
    // restore the dump
    falkor.restore_db(size).await?;
    // start falkor
    let falkor = falkor.start().await?;

    // get the graph size
    let (node_count, relation_count) = falkor.graph_size().await?;

    info!(
        "graph has {} nodes and {} relations",
        format_number(node_count),
        format_number(relation_count)
    );
    info!("running {} queries", format_number(number_of_queries));

    // prepare the mpsc channel
    let (tx, rx) = tokio::sync::mpsc::channel::<PreparedQuery>(2000 * parallel);
    let rx: Arc<Mutex<Receiver<PreparedQuery>>> = Arc::new(Mutex::new(rx));

    let mut filler_handles = Vec::with_capacity(2);
    // iterate over queries and send them to the workers
    filler_handles.push(fill_queries(number_of_queries, tx.clone())?);
    filler_handles.push(fill_queries(number_of_queries, tx.clone())?);

    let mut workers_handles = Vec::with_capacity(parallel);
    // start workers
    let start = Instant::now();
    for spawn_id in 0..parallel {
        let handle = spawn_worker(&falkor, spawn_id, &rx).await?;
        workers_handles.push(handle);
    }

    // wait for the filler to finish drop the transmission side
    // to let the workers know that they should stop
    for handle in filler_handles {
        let _ = handle.await;
    }
    drop(tx);

    for handle in workers_handles {
        let _ = handle.await;
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

fn fill_queries(
    number_of_queries: u64,
    tx: Sender<PreparedQuery>,
) -> BenchmarkResult<JoinHandle<()>> {
    Ok(tokio::spawn({
        async move {
            let queries_repository =
                benchmark::queries_repository::UsersQueriesRepository::new(9998, 121716);
            let queries: Box<dyn Iterator<Item = PreparedQuery> + Send + Sync> =
                queries_repository.random_queries(number_of_queries);

            for query in queries {
                if let Err(e) = tx.send(query).await {
                    error!("error filling query: {}, exiting", e);
                    return;
                }
            }
            info!("fill_queries finished");
        }
    }))
}

#[instrument(skip(falkor), fields(vendor = "falkor"))]
async fn spawn_worker(
    falkor: &Falkor<Started>,
    worker_id: usize,
    receiver: &Arc<Mutex<Receiver<PreparedQuery>>>,
) -> BenchmarkResult<JoinHandle<()>> {
    info!("spawning worker");
    let mut client = falkor.client().await?;
    let receiver = Arc::clone(&receiver);

    let handle = tokio::spawn({
        async move {
            let worker_id = worker_id.to_string();
            let worker_id_str = worker_id.as_str();
            let mut counter = 0u32;
            loop {
                // get next value and release the mutex
                let received = receiver.lock().await.recv().await;

                match received {
                    Some(prepared_query) => {
                        let r = client
                            .execute_prepared_query(worker_id_str, &prepared_query)
                            .await;
                        match r {
                            Ok(_) => {
                                counter += 1;
                                // info!("worker {} processed query {}", worker_id, counter);
                                if counter % 1000 == 0 {
                                    info!("worker {} processed {} queries", worker_id, counter);
                                }
                            }
                            // in case of error sleep for 3 seconds, that will give the benchmark some time to
                            // accumulate more queries for the time that the system recovers.
                            Err(_) => {
                                let seconds_wait = 3u64;
                                info!(
                                    "worker {} failed to process query, sleeping for {} seconds",
                                    worker_id, seconds_wait
                                );
                                tokio::time::sleep(Duration::from_secs(seconds_wait)).await;
                            }
                        }
                    }
                    None => {
                        break;
                    }
                }
            }
            info!("worker {} finished", worker_id);
        }
    });

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
        ._execute_query(
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
                    ._execute_query("loader", "", query.as_str())
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
