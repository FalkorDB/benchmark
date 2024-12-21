use benchmark::cli::Cli;
use benchmark::cli::Commands;
use benchmark::cli::Commands::GenerateAutoComplete;
use benchmark::error::BenchmarkError::OtherError;
use benchmark::error::BenchmarkResult;
use benchmark::falkor::{Falkor, Started, Stopped};
use benchmark::neo4j_client::Neo4jClient;
use benchmark::queries_repository::PreparedQuery;
use benchmark::scenario::Name::Users;
use benchmark::scenario::{Size, Spec, Vendor};
use benchmark::scheduler::Msg;
use benchmark::utils::{delete_file, file_exists, format_number};
use benchmark::{
    scheduler, FALKOR_ERROR_REQUESTS_DURATION_HISTOGRAM,
    FALKOR_SUCCESS_REQUESTS_DURATION_HISTOGRAM, NEO4J_ERROR_REQUESTS_DURATION_HISTOGRAM,
    NEO4J_SUCCESS_REQUESTS_DURATION_HISTOGRAM,
};
use clap::{Command, CommandFactory, Parser};
use clap_complete::{generate, Generator};
use futures::StreamExt;
use histogram::Histogram;
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc::Receiver;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info, instrument};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    let mut cmd = Cli::command();
    let cli = Cli::parse();

    let filter = EnvFilter::from_default_env().add_directive(LevelFilter::INFO.into());
    let subscriber = fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .with_env_filter(filter);

    subscriber.init();

    let prometheus_endpoint = benchmark::prometheus_endpoint::PrometheusEndpoint::default();

    match cli.command {
        GenerateAutoComplete { shell } => {
            eprintln!("Generating completion file for {shell}...");
            print_completions(shell, &mut cmd);
        }

        Commands::Load {
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
            parallel,
            name,
            mps,
            simulate,
        } => match vendor {
            Vendor::Neo4j => {
                run_neo4j(parallel, name, mps, simulate).await?;
            }
            Vendor::Falkor => {
                run_falkor(parallel, name, mps, simulate).await?;
            }
        },

        Commands::GenerateQueries {
            size,
            dataset,
            name,
            write_ratio,
        } => {
            prepare_queries(dataset, size, name, write_ratio).await?;
        }
    }
    drop(prometheus_endpoint);
    Ok(())
}

async fn run_neo4j(
    parallel: usize,
    file_name: String,
    mps: usize,
    simulate: Option<usize>,
) -> BenchmarkResult<()> {
    let mut neo4j = benchmark::neo4j::Neo4j::default();
    // stop neo4j if it is running
    neo4j.stop(false).await?;
    let (queries_metadata, queries) = read_queries(file_name).await?;
    let number_of_queries = queries_metadata.size;
    let spec = Spec::new(Users, queries_metadata.dataset, Vendor::Neo4j);
    neo4j.restore_db(spec).await?;
    // start neo4j
    neo4j.start().await?;
    let client = neo4j.client().await?;
    info!("client connected to neo4j");
    // get the graph size
    let (node_count, relation_count) = client.graph_size().await?;

    info!(
        "graph has {} nodes and {} relations",
        format_number(node_count),
        format_number(relation_count)
    );
    info!(
        "running {} queries",
        format_number(number_of_queries as u64)
    );
    // prepare the mpsc channel
    let (tx, rx) = tokio::sync::mpsc::channel::<Msg<PreparedQuery>>(20 * parallel);
    let rx: Arc<Mutex<Receiver<Msg<PreparedQuery>>>> = Arc::new(Mutex::new(rx));
    let scheduler_handle = scheduler::spawn_scheduler::<PreparedQuery>(mps, tx.clone(), queries);
    let mut workers_handles = Vec::with_capacity(parallel);

    let start = Instant::now();
    for spawn_id in 0..parallel {
        let handle = spawn_neo4j_worker(client.clone(), spawn_id, &rx, simulate).await?;
        workers_handles.push(handle);
    }
    let _ = scheduler_handle.await;
    drop(tx);

    for handle in workers_handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();

    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries as u64),
        elapsed
    );
    neo4j.stop(true).await?;
    // stop neo4j
    // write the report
    Ok(())
}

async fn spawn_neo4j_worker(
    client: Neo4jClient,
    worker_id: usize,
    receiver: &Arc<Mutex<Receiver<Msg<PreparedQuery>>>>,
    simulate: Option<usize>,
) -> BenchmarkResult<JoinHandle<()>> {
    info!("spawning worker");
    let receiver = Arc::clone(receiver);
    let handle = tokio::spawn(async move {
        let worker_id = worker_id.to_string();
        let worker_id_str = worker_id.as_str();
        let mut counter = 0u32;
        let mut client = client.clone();
        loop {
            // get the next value and release the mutex
            let received = receiver.lock().await.recv().await;

            match received {
                Some(prepared_query) => {
                    let start_time = Instant::now();

                    let r = client
                        .execute_prepared_query(worker_id_str, &prepared_query, &simulate)
                        .await;
                    let duration = start_time.elapsed();
                    match r {
                        Ok(_) => {
                            NEO4J_SUCCESS_REQUESTS_DURATION_HISTOGRAM
                                .observe(duration.as_secs_f64());
                            counter += 1;
                            if counter % 1000 == 0 {
                                info!("worker {} processed {} queries", worker_id, counter);
                            }
                        }
                        Err(e) => {
                            NEO4J_ERROR_REQUESTS_DURATION_HISTOGRAM.observe(duration.as_secs_f64());
                            let seconds_wait = 3u64;
                            info!(
                                "worker {} failed to process query, not sleeping for {} seconds {:?}",
                                worker_id, seconds_wait, e
                            );
                        }
                    }
                }
                None => {
                    info!("worker {} received None, exiting", worker_id);
                    break;
                }
            }
        }
        info!("worker {} finished", worker_id);
    });

    Ok(handle)
}
#[instrument]
async fn run_falkor(
    parallel: usize,
    file_name: String,
    mps: usize,
    simulate: Option<usize>,
) -> BenchmarkResult<()> {
    if parallel == 0 {
        return Err(OtherError(
            "Parallelism level must be greater than zero.".to_string(),
        ));
    }
    let falkor: Falkor<Stopped> = benchmark::falkor::Falkor::default();

    let (queries_metadata, queries) = read_queries(file_name).await?;

    // if dump not present return error
    falkor
        .dump_exists_or_error(queries_metadata.dataset)
        .await?;
    // restore the dump
    falkor.restore_db(queries_metadata.dataset).await?;
    // start falkor
    let falkor = falkor.start().await?;

    // get the graph size
    let (node_count, relation_count) = falkor.graph_size().await?;

    info!(
        "graph has {} nodes and {} relations",
        format_number(node_count),
        format_number(relation_count)
    );

    // prepare the mpsc channel
    let (tx, rx) = tokio::sync::mpsc::channel::<Msg<PreparedQuery>>(20 * parallel);
    let rx: Arc<Mutex<Receiver<Msg<PreparedQuery>>>> = Arc::new(Mutex::new(rx));

    // iterate over queries and send them to the workers

    let number_of_queries = queries_metadata.size;
    info!(
        "running {} queries",
        format_number(number_of_queries as u64)
    );

    let scheduler_handle = scheduler::spawn_scheduler::<PreparedQuery>(mps, tx.clone(), queries);
    let mut workers_handles = Vec::with_capacity(parallel);
    // start workers
    let start = Instant::now();
    for spawn_id in 0..parallel {
        let handle = spawn_falkor_worker(&falkor, spawn_id, &rx, simulate).await?;
        workers_handles.push(handle);
    }

    let _ = scheduler_handle.await;
    drop(tx);

    for handle in workers_handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries as u64),
        elapsed
    );

    // stop falkor
    let _stopped = falkor.stop().await?;
    Ok(())
}

async fn spawn_falkor_worker(
    falkor: &Falkor<Started>,
    worker_id: usize,
    receiver: &Arc<Mutex<Receiver<Msg<PreparedQuery>>>>,
    simulate: Option<usize>,
) -> BenchmarkResult<JoinHandle<()>> {
    info!("spawning worker");
    let mut client = falkor.client().await?;
    let receiver = Arc::clone(receiver);
    let handle = tokio::spawn(async move {
        let worker_id = worker_id.to_string();
        let worker_id_str = worker_id.as_str();
        let mut counter = 0u32;
        loop {
            // get the next value and release the mutex
            let received = receiver.lock().await.recv().await;

            match received {
                Some(prepared_query) => {
                    let start_time = Instant::now();

                    let r = client
                        .execute_prepared_query(worker_id_str, &prepared_query, &simulate)
                        .await;
                    let duration = start_time.elapsed();
                    match r {
                        Ok(_) => {
                            FALKOR_SUCCESS_REQUESTS_DURATION_HISTOGRAM
                                .observe(duration.as_secs_f64());
                            counter += 1;
                            if counter % 1000 == 0 {
                                info!("worker {} processed {} queries", worker_id, counter);
                            }
                        }
                        Err(e) => {
                            FALKOR_ERROR_REQUESTS_DURATION_HISTOGRAM
                                .observe(duration.as_secs_f64());
                            let seconds_wait = 3u64;
                            info!(
                                "worker {} failed to process query, not sleeping for {} seconds {:?}",
                                worker_id, seconds_wait, e
                            );
                        }
                    }
                }
                None => {
                    info!("worker {} received None, exiting", worker_id);
                    break;
                }
            }
        }
        info!("worker {} finished", worker_id);
    });

    Ok(handle)
}
async fn init_falkor(
    size: Size,
    _force: bool,
) -> BenchmarkResult<()> {
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Neo4j);
    let falkor = benchmark::falkor::Falkor::default();
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
    let mut neo4j = benchmark::neo4j::Neo4j::default();
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
        info!("stopping neo4j and deleting database neo4j");
        neo4j.stop(false).await?;
        neo4j.clean_db().await?;
        neo4j.stop(true).await?;
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
    neo4j.stop(true).await?;
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

#[derive(Debug, Serialize, Deserialize)]
struct PrepareQueriesMetadata {
    size: usize,
    dataset: Size,
}
async fn prepare_queries(
    dataset: Size,
    size: usize,
    file_name: String,
    write_ratio: f32,
) -> BenchmarkResult<()> {
    let metadata = PrepareQueriesMetadata { size, dataset };
    let start = Instant::now();
    let queries_repository =
        benchmark::queries_repository::UsersQueriesRepository::new(9998, 121716);
    let queries = Box::new(queries_repository.random_queries(size, write_ratio));

    let file = File::create(file_name).await?;
    let mut writer = BufWriter::new(file);
    let metadata_line = serde_json::to_string(&metadata)?;
    writer.write_all(metadata_line.as_bytes()).await?;
    writer.write_all(b"\n").await?;

    for query in queries {
        let json_string = serde_json::to_string(&query)?;
        writer.write_all(json_string.as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }
    writer.flush().await?;

    let duration = start.elapsed();
    info!("Time taken to prepare queries: {:?}", duration);
    Ok(())
}

async fn read_queries(
    file_name: String
) -> BenchmarkResult<(PrepareQueriesMetadata, Vec<PreparedQuery>)> {
    let start = Instant::now();
    let file = File::open(file_name).await?;
    let mut reader = BufReader::new(file);

    // the first line is PrepareQueriesMetadata read it
    let mut metadata_line = String::new();
    reader.read_line(&mut metadata_line).await?;

    match serde_json::from_str::<PrepareQueriesMetadata>(&metadata_line) {
        Ok(metadata) => {
            let size = metadata.size;
            let mut queries = Vec::with_capacity(size);
            let mut lines = reader.lines();

            while let Some(line) = lines.next_line().await? {
                let query: PreparedQuery = serde_json::from_str(&line)?;
                queries.push(query);
            }
            let duration = start.elapsed();
            info!("Reading {} queries took {:?}", size, duration);
            Ok((metadata, queries))
        }
        Err(e) => Err(OtherError(format!("Error parsing metadata: {}", e))),
    }
}
