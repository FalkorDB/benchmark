use benchmark::cli::Cli;
use benchmark::cli::Commands;
use benchmark::cli::Commands::GenerateAutoComplete;
use benchmark::error::BenchmarkError::OtherError;
use benchmark::error::BenchmarkResult;
use benchmark::falkor::{Falkor, Started, Stopped};
use benchmark::memgraph_client::MemgraphClient;
use benchmark::neo4j_client::Neo4jClient;
use benchmark::queries_repository::{PreparedQuery, QueryCatalogEntry};
use benchmark::scenario::Name::Users;
use benchmark::scenario::{Size, Spec, Vendor};
use benchmark::scheduler::Msg;
use benchmark::utils::{
    create_directory_if_not_exists, delete_file, file_exists, format_number, write_to_file,
};
use benchmark::{
    scheduler, FALKOR_ERROR_REQUESTS_DURATION_HISTOGRAM, FALKOR_LATENCY_P50_US,
    FALKOR_LATENCY_P95_US, FALKOR_LATENCY_P99_US, FALKOR_QUERY_LATENCY_PCT_US,
    FALKOR_SUCCESS_REQUESTS_DURATION_HISTOGRAM, MEMGRAPH_ERROR_REQUESTS_DURATION_HISTOGRAM,
    MEMGRAPH_LATENCY_P50_US, MEMGRAPH_LATENCY_P95_US, MEMGRAPH_LATENCY_P99_US,
    MEMGRAPH_QUERY_LATENCY_PCT_US, MEMGRAPH_SUCCESS_REQUESTS_DURATION_HISTOGRAM,
    NEO4J_ERROR_REQUESTS_DURATION_HISTOGRAM, NEO4J_LATENCY_P50_US, NEO4J_LATENCY_P95_US,
    NEO4J_LATENCY_P99_US, NEO4J_QUERY_LATENCY_PCT_US, NEO4J_SUCCESS_REQUESTS_DURATION_HISTOGRAM,
};
use clap::{Command, CommandFactory, Parser};
use clap_complete::{generate, Generator};
use futures::StreamExt;
use histogram::Histogram;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use prometheus::{Encoder, TextEncoder};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc::Receiver;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info, instrument, trace};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{fmt, EnvFilter};
mod aggregator;

use url::Url;

fn default_results_dir() -> String {
    use time::macros::format_description;

    // YYMMDD-HH:MM (UTC)
    let fmt = format_description!("[year repr:last_two][month padding:zero][day padding:zero]-[hour padding:zero]:[minute padding:zero]");

    let ts = time::OffsetDateTime::now_utc()
        .format(&fmt)
        .unwrap_or_else(|_| "000000-00:00".to_string());

    format!("Results-{}", ts)
}

fn redact_endpoint(endpoint: &str) -> String {
    // Best-effort: if this isn't a valid URL, just return a placeholder.
    if let Ok(mut url) = Url::parse(endpoint) {
        // Strip password if present; keep username to help identify which creds are being used.
        let _ = url.set_password(None);
        return url.to_string();
    }
    "<invalid-endpoint>".to_string()
}

/// Parse Neo4j endpoint string into (uri, user, password, database)
/// Supports formats like:
/// - neo4j://user:pass@host:7687
/// - bolt://user:pass@host:7687
/// - neo4j://host:7687 (uses default credentials)
fn parse_neo4j_endpoint(
    endpoint: &str
) -> BenchmarkResult<(String, String, String, Option<String>)> {
    let url = Url::parse(endpoint)
        .map_err(|e| OtherError(format!("Invalid Neo4j endpoint URL '{}': {}", endpoint, e)))?;

    // Validate scheme
    match url.scheme() {
        "neo4j" | "bolt" | "neo4j+s" | "bolt+s" => {}
        scheme => {
            return Err(OtherError(format!(
                "Unsupported Neo4j scheme '{}'. Use neo4j://, bolt://, neo4j+s://, or bolt+s://",
                scheme
            )));
        }
    }

    // Extract host and port
    let host = url
        .host_str()
        .ok_or_else(|| OtherError(format!("No host found in Neo4j endpoint: {}", endpoint)))?;

    let port = url.port().unwrap_or(7687); // Default Neo4j port

    // Build URI (neo4rs expects format like "127.0.0.1:7687")
    let uri = format!("{}:{}", host, port);

    // Extract credentials.
    // If missing from URL, fall back to env vars so users don't need to embed secrets in endpoints.
    let user = if !url.username().is_empty() {
        url.username().to_string()
    } else {
        std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string())
    };

    let password = if let Some(pw) = url.password() {
        pw.to_string()
    } else {
        std::env::var("NEO4J_PASSWORD").unwrap_or_else(|_| "password".to_string())
    };

    // Default database name for Neo4j
    let database = Some("neo4j".to_string());

    Ok((uri, user, password, database))
}

/// Parse Memgraph endpoint string into (uri, user, password, database)
/// Supports formats like:
/// - bolt://user:pass@host:7687
/// - memgraph://user:pass@host:7687
/// - bolt://host:7687 (uses empty credentials for Memgraph)
fn parse_memgraph_endpoint(
    endpoint: &str
) -> BenchmarkResult<(String, String, String, Option<String>)> {
    let url = Url::parse(endpoint).map_err(|e| {
        OtherError(format!(
            "Invalid Memgraph endpoint URL '{}': {}",
            endpoint, e
        ))
    })?;

    // Validate scheme
    match url.scheme() {
        "bolt" | "bolt+s" | "memgraph" | "memgraph+s" => {}
        scheme => {
            return Err(OtherError(format!(
                "Unsupported Memgraph scheme '{}'. Use bolt://, memgraph://, bolt+s://, or memgraph+s://",
                scheme
            )));
        }
    }

    // Extract host and port
    let host = url
        .host_str()
        .ok_or_else(|| OtherError(format!("No host found in Memgraph endpoint: {}", endpoint)))?;

    let port = url.port().unwrap_or(7687); // Default Memgraph port

    // Build URI (format like "127.0.0.1:7687")
    let uri = format!("{}:{}", host, port);

    // Extract credentials.
    // If missing from URL, fall back to env vars so users don't need to embed secrets in endpoints.
    let user = if !url.username().is_empty() {
        url.username().to_string()
    } else {
        std::env::var("MEMGRAPH_USER").unwrap_or_else(|_| String::new())
    };

    let password = if let Some(pw) = url.password() {
        pw.to_string()
    } else {
        std::env::var("MEMGRAPH_PASSWORD").unwrap_or_else(|_| String::new())
    };

    Ok((uri, user, password, Some("memgraph".to_string())))
}

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
            batch_size,
            endpoint,
        } => {
            // Expose metrics while running load operations.
            let _prometheus_endpoint =
                benchmark::prometheus_endpoint::PrometheusEndpoint::default();

            info!(
                "Init benchmark {} {} {} (batch_size: {})",
                vendor, size, force, batch_size
            );
            match vendor {
                Vendor::Neo4j => {
                    if dry_run {
                        dry_init_neo4j(size, batch_size).await?;
                    } else {
                        init_neo4j(size, force, batch_size, endpoint).await?;
                    }
                }
                Vendor::Falkor => {
                    if dry_run {
                        info!("Dry run");
                        todo!()
                    } else {
                        init_falkor(size, force, batch_size, endpoint).await?;
                    }
                }
                Vendor::Memgraph => {
                    if dry_run {
                        dry_init_memgraph(size, batch_size).await?;
                    } else {
                        init_memgraph(size, force, batch_size, endpoint).await?;
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
            endpoint,
            results_dir,
        } => {
            // Expose metrics while running benchmarks.
            let _prometheus_endpoint =
                benchmark::prometheus_endpoint::PrometheusEndpoint::default();

            // Always store results; if user didn't provide a directory, generate one.
            let results_dir = Some(results_dir.unwrap_or_else(default_results_dir));
            match vendor {
                Vendor::Neo4j => {
                    run_neo4j(parallel, name, mps, simulate, endpoint, results_dir).await?;
                }
                Vendor::Falkor => {
                    run_falkor(parallel, name, mps, simulate, endpoint, results_dir).await?;
                }
                Vendor::Memgraph => {
                    run_memgraph(parallel, name, mps, simulate, endpoint, results_dir).await?;
                }
            }
        }

        Commands::GenerateQueries {
            size,
            dataset,
            name,
            write_ratio,
        } => {
            prepare_queries(dataset, size, name, write_ratio).await?;
        }
        Commands::Aggregate {
            results_dir,
            out_dir,
        } => {
            aggregator::aggregate_results(&results_dir, &out_dir)?;
        }
    }
    Ok(())
}

fn percentile_us(
    hist: &histogram::Histogram,
    p: f64,
) -> u64 {
    hist.percentile(p)
        .ok()
        .flatten()
        .map(|b| b.end())
        .unwrap_or(0)
}

const QUERY_HIST_PCTS: [f64; 11] = [
    10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 95.0, 99.0,
];

struct PerQueryLatency {
    // Indexed by q_id.
    catalog: Vec<QueryCatalogEntry>,
    hists: Vec<std::sync::Mutex<histogram::Histogram>>,
}

impl PerQueryLatency {
    fn new(catalog: Vec<QueryCatalogEntry>) -> BenchmarkResult<Self> {
        let mut hists = Vec::with_capacity(catalog.len());
        for _ in 0..catalog.len() {
            hists.push(std::sync::Mutex::new(histogram::Histogram::new(7, 64)?));
        }
        Ok(Self { catalog, hists })
    }

    fn record_us(
        &self,
        q_id: u16,
        us: u64,
    ) {
        let idx = q_id as usize;
        let Some(m) = self.hists.get(idx) else {
            return;
        };
        if let Ok(mut h) = m.lock() {
            let _ = h.increment(us);
        }
    }

    fn export_to_prometheus(
        &self,
        vendor: Vendor,
    ) {
        // Clear old label values in case multiple runs happen in a single process.
        match vendor {
            Vendor::Falkor => FALKOR_QUERY_LATENCY_PCT_US.reset(),
            Vendor::Neo4j => NEO4J_QUERY_LATENCY_PCT_US.reset(),
            Vendor::Memgraph => MEMGRAPH_QUERY_LATENCY_PCT_US.reset(),
        }

        for entry in &self.catalog {
            let idx = entry.id as usize;
            let Some(m) = self.hists.get(idx) else {
                continue;
            };
            let Ok(h) = m.lock() else {
                continue;
            };

            // Skip empty hists.
            if percentile_us(&h, 50.0) == 0 {
                continue;
            }

            for pct in QUERY_HIST_PCTS {
                let v = percentile_us(&h, pct) as i64;
                let pct_label = if (pct - pct.round()).abs() < f64::EPSILON {
                    format!("{}", pct as i64)
                } else {
                    format!("{}", pct)
                };

                match vendor {
                    Vendor::Falkor => {
                        FALKOR_QUERY_LATENCY_PCT_US
                            .with_label_values(&[entry.name.as_str(), pct_label.as_str()])
                            .set(v);
                    }
                    Vendor::Neo4j => {
                        NEO4J_QUERY_LATENCY_PCT_US
                            .with_label_values(&[entry.name.as_str(), pct_label.as_str()])
                            .set(v);
                    }
                    Vendor::Memgraph => {
                        MEMGRAPH_QUERY_LATENCY_PCT_US
                            .with_label_values(&[entry.name.as_str(), pct_label.as_str()])
                            .set(v);
                    }
                }
            }
        }
    }
}

async fn run_neo4j(
    parallel: usize,
    file_name: String,
    mps: usize,
    simulate: Option<usize>,
    endpoint: Option<String>,
    results_dir: Option<String>,
) -> BenchmarkResult<()> {
    let queries_file = file_name.clone();
    let (queries_metadata, queries) = read_queries(file_name).await?;
    let number_of_queries = queries_metadata.size;

    let client = if let Some(ref endpoint_str) = endpoint {
        info!(
            "Using external Neo4j endpoint: {}",
            redact_endpoint(endpoint_str)
        );
        // Parse the endpoint and create client directly
        let (uri, user, password, database) = parse_neo4j_endpoint(endpoint_str)?;
        benchmark::neo4j_client::Neo4jClient::new(uri, user, password, database).await?
    } else {
        // Use local Neo4j instance (existing behavior)
        let mut neo4j = benchmark::neo4j::Neo4j::default();
        // stop neo4j if it is running
        neo4j.stop(false).await?;
        let spec = Spec::new(Users, queries_metadata.dataset, Vendor::Neo4j);
        neo4j.restore_db(spec).await?;
        // start neo4j
        neo4j.start().await?;
        neo4j.client().await?
    };
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

    // HDR histogram for accurate pXX latencies (microseconds)
    let latency_hist = Arc::new(tokio::sync::Mutex::new(histogram::Histogram::new(7, 64)?));

    // Per-query histograms for "single"-style percentiles (P10..P99)
    let per_query = Arc::new(PerQueryLatency::new(queries_metadata.catalog.clone())?);

    let started_at = SystemTime::now();
    let start = Instant::now();
    for spawn_id in 0..parallel {
        let handle = spawn_neo4j_worker(
            client.clone(),
            spawn_id,
            &rx,
            simulate,
            latency_hist.clone(),
            per_query.clone(),
        )
        .await?;
        workers_handles.push(handle);
    }
    let _ = scheduler_handle.await;
    drop(tx);

    for handle in workers_handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    let finished_at = SystemTime::now();

    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries as u64),
        elapsed
    );

    // Export accurate pXX latency gauges (microseconds)
    {
        let hist = latency_hist.lock().await;
        NEO4J_LATENCY_P50_US.set(percentile_us(&hist, 50.0) as i64);
        NEO4J_LATENCY_P95_US.set(percentile_us(&hist, 95.0) as i64);
        NEO4J_LATENCY_P99_US.set(percentile_us(&hist, 99.0) as i64);
    }

    // Export per-query percentiles.
    per_query.export_to_prometheus(Vendor::Neo4j);

    write_run_results(
        results_dir,
        Vendor::Neo4j,
        queries_metadata.dataset,
        &queries_file,
        parallel,
        mps,
        simulate,
        &endpoint,
        number_of_queries,
        started_at,
        finished_at,
        elapsed,
    )
    .await?;
    // Only stop neo4j if we're managing a local instance
    if endpoint.is_none() {
        // We need to get the neo4j instance back to stop it
        // For now, we'll skip stopping for external endpoints
        info!("Using external endpoint, skipping Neo4j process management");
    }
    Ok(())
}

async fn spawn_neo4j_worker(
    client: Neo4jClient,
    worker_id: usize,
    receiver: &Arc<Mutex<Receiver<Msg<PreparedQuery>>>>,
    simulate: Option<usize>,
    latency_hist: Arc<tokio::sync::Mutex<histogram::Histogram>>,
    per_query: Arc<PerQueryLatency>,
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
                            // Accurate percentile source
                            {
                                let mut h = latency_hist.lock().await;
                                let _ = h.increment(duration.as_micros() as u64);
                            }
                            // Per-query latency tracking
                            per_query.record_us(
                                prepared_query.payload.q_id,
                                duration.as_micros() as u64,
                            );
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
    endpoint: Option<String>,
    results_dir: Option<String>,
) -> BenchmarkResult<()> {
    if parallel == 0 {
        return Err(OtherError(
            "Parallelism level must be greater than zero.".to_string(),
        ));
    }
    let falkor: Falkor<Stopped> = benchmark::falkor::Falkor::new_with_endpoint(endpoint.clone());

    let queries_file = file_name.clone();
    let (queries_metadata, queries) = read_queries(file_name).await?;

    // if external endpoint, skip dump operations
    if endpoint.is_none() {
        // if dump not present, initialize the database
        if falkor
            .dump_exists_or_error(queries_metadata.dataset)
            .await
            .is_err()
        {
            info!("Dump file not found, initializing falkor database...");
            init_falkor(queries_metadata.dataset, false, 1000, endpoint.clone()).await?;
        }
        // restore the dump
        falkor.restore_db(queries_metadata.dataset).await?;
    } else {
        info!("Using external endpoint, skipping dump restore operations");
    }
    // start falkor
    let falkor = falkor.start().await?;

    // get the graph size
    let (node_count, relation_count) = falkor.graph_size().await?;

    // Best-effort graph memory reporting (query-interface metric).
    falkor.collect_graph_memory_usage_metrics().await;

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

    // HDR histogram for accurate pXX latencies (microseconds)
    let latency_hist = Arc::new(tokio::sync::Mutex::new(histogram::Histogram::new(7, 64)?));

    // Per-query histograms for "single"-style percentiles (P10..P99)
    let per_query = Arc::new(PerQueryLatency::new(queries_metadata.catalog.clone())?);

    let started_at = SystemTime::now();
    // start workers
    let start = Instant::now();
    for spawn_id in 0..parallel {
        let handle = spawn_falkor_worker(
            &falkor,
            spawn_id,
            &rx,
            simulate,
            latency_hist.clone(),
            per_query.clone(),
        )
        .await?;
        workers_handles.push(handle);
    }

    let _ = scheduler_handle.await;
    drop(tx);

    for handle in workers_handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    let finished_at = SystemTime::now();

    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries as u64),
        elapsed
    );

    // Export accurate pXX latency gauges (microseconds)
    {
        let hist = latency_hist.lock().await;
        FALKOR_LATENCY_P50_US.set(percentile_us(&hist, 50.0) as i64);
        FALKOR_LATENCY_P95_US.set(percentile_us(&hist, 95.0) as i64);
        FALKOR_LATENCY_P99_US.set(percentile_us(&hist, 99.0) as i64);
    }

    // Export per-query percentiles.
    per_query.export_to_prometheus(Vendor::Falkor);

    write_run_results(
        results_dir,
        Vendor::Falkor,
        queries_metadata.dataset,
        &queries_file,
        parallel,
        mps,
        simulate,
        &endpoint,
        number_of_queries,
        started_at,
        finished_at,
        elapsed,
    )
    .await?;

    // stop falkor
    let _stopped = falkor.stop().await?;
    Ok(())
}

async fn spawn_falkor_worker(
    falkor: &Falkor<Started>,
    worker_id: usize,
    receiver: &Arc<Mutex<Receiver<Msg<PreparedQuery>>>>,
    simulate: Option<usize>,
    latency_hist: Arc<tokio::sync::Mutex<histogram::Histogram>>,
    per_query: Arc<PerQueryLatency>,
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
                            // Accurate percentile source
                            {
                                let mut h = latency_hist.lock().await;
                                let _ = h.increment(duration.as_micros() as u64);
                            }
                            // Per-query latency tracking
                            per_query.record_us(
                                prepared_query.payload.q_id,
                                duration.as_micros() as u64,
                            );
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
    batch_size: usize,
    endpoint: Option<String>,
) -> BenchmarkResult<()> {
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Neo4j);
    let falkor = benchmark::falkor::Falkor::new_with_endpoint(endpoint.clone());
    if endpoint.is_none() {
        falkor.clean_db().await?;
    }

    let falkor = falkor.start().await?;
    info!("writing index and data");
    // let index_iterator = spec.init_index_iterator().await?;
    let start = Instant::now();

    let mut falkor_client = falkor.client().await?;

    // Create index with graceful handling of "already exists" error
    falkor_client
        .create_index_if_not_exists(
            "main",
            "create_index",
            "CREATE INDEX FOR (u:User) ON (u.id)",
        )
        .await?;

    let mut data_iterator = spec.init_data_iterator().await?;

    info!("Loading data in batches of {} commands", batch_size);

    let mut current_batch = Vec::with_capacity(batch_size);
    let mut total_processed = 0;
    let mut batch_count = 0;
    let start_time = tokio::time::Instant::now();
    let mut last_progress_report = start_time;
    const PROGRESS_INTERVAL_SECS: u64 = 5;

    while let Some(result) = data_iterator.next().await {
        match result {
            Ok(query) => {
                current_batch.push(query);
                total_processed += 1;

                if current_batch.len() >= batch_size {
                    batch_count += 1;
                    let batch_start = tokio::time::Instant::now();

                    info!(
                        "Processing batch {} with {} items (total processed: {})",
                        batch_count,
                        current_batch.len(),
                        total_processed
                    );

                    falkor_client
                        .execute_batch("loader", &current_batch)
                        .await?;
                    current_batch = Vec::with_capacity(batch_size);

                    let batch_duration = batch_start.elapsed();
                    trace!("Batch {} completed in {:?}", batch_count, batch_duration);

                    // Report progress every 5 seconds
                    let now = tokio::time::Instant::now();
                    if now.duration_since(last_progress_report).as_secs() >= PROGRESS_INTERVAL_SECS
                    {
                        let elapsed = now.duration_since(start_time);
                        let rate = total_processed as f64 / elapsed.as_secs_f64();
                        info!("Progress: {} items processed in {:?} ({:.2} items/sec, {} batches completed)", 
                              format_number(total_processed as u64), elapsed, rate, batch_count);
                        last_progress_report = now;
                    }
                }
            }
            Err(e) => {
                error!("Error processing stream item: {:?}", e);
            }
        }
    }

    // Process remaining items if any
    if !current_batch.is_empty() {
        batch_count += 1;
        info!(
            "Processing final batch {} with {} items",
            batch_count,
            current_batch.len()
        );
        falkor_client
            .execute_batch("loader", &current_batch)
            .await?;
    }

    let total_duration = start_time.elapsed();
    let final_rate = total_processed as f64 / total_duration.as_secs_f64();
    info!(
        "Completed processing {} items in {} batches over {:?} (avg {:.2} items/sec)",
        format_number(total_processed as u64),
        batch_count,
        total_duration,
        final_rate
    );

    let (node_count, relation_count) = falkor.graph_size().await?;
    info!(
        "{} nodes and {} relations were imported at {:?}",
        format_number(node_count),
        format_number(relation_count),
        start.elapsed()
    );
    info!("writing done, took: {:?}", start.elapsed());
    let falkor = falkor.stop().await?;
    if endpoint.is_none() {
        falkor.save_db(size).await?;
    }

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

#[derive(Debug, Serialize)]
struct RunResultsMeta {
    vendor: String,
    dataset: String,
    queries_file: String,
    queries_count: usize,
    parallel: usize,
    mps: usize,
    simulate_ms: Option<usize>,
    endpoint: Option<String>,
    started_at_epoch_secs: u64,
    finished_at_epoch_secs: u64,
    elapsed_ms: u128,
}

fn system_time_epoch_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

async fn write_run_results(
    results_dir: Option<String>,
    vendor: Vendor,
    dataset: Size,
    queries_file: &str,
    parallel: usize,
    mps: usize,
    simulate: Option<usize>,
    endpoint: &Option<String>,
    queries_count: usize,
    started_at: SystemTime,
    finished_at: SystemTime,
    elapsed: Duration,
) -> BenchmarkResult<()> {
    let Some(base_dir) = results_dir else {
        return Ok(());
    };

    let vendor_dir = PathBuf::from(base_dir).join(vendor.to_string());
    let vendor_dir_str = vendor_dir.to_string_lossy().to_string();
    create_directory_if_not_exists(&vendor_dir_str).await?;

    let meta = RunResultsMeta {
        vendor: vendor.to_string(),
        dataset: dataset.to_string(),
        queries_file: queries_file.to_string(),
        queries_count,
        parallel,
        mps,
        simulate_ms: simulate,
        endpoint: endpoint.as_ref().map(|e| redact_endpoint(e)),
        started_at_epoch_secs: system_time_epoch_secs(started_at),
        finished_at_epoch_secs: system_time_epoch_secs(finished_at),
        elapsed_ms: elapsed.as_millis(),
    };

    let meta_json = serde_json::to_string_pretty(&meta)?;
    let meta_path = vendor_dir.join("meta.json").to_string_lossy().to_string();
    write_to_file(&meta_path, &meta_json).await?;

    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(|e| OtherError(format!("Failed to encode prometheus metrics: {}", e)))?;
    let metrics_text = String::from_utf8_lossy(&buffer).to_string();

    let metrics_path = vendor_dir
        .join("metrics.prom")
        .to_string_lossy()
        .to_string();
    write_to_file(&metrics_path, &metrics_text).await?;

    info!("Wrote run results to {}", vendor_dir_str);

    Ok(())
}

async fn dry_init_neo4j(
    size: Size,
    _batch_size: usize,
) -> BenchmarkResult<()> {
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
    batch_size: usize,
    endpoint: Option<String>,
) -> BenchmarkResult<()> {
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Neo4j);

    let client = if let Some(ref endpoint_str) = endpoint {
        info!(
            "Using external Neo4j endpoint for data loading: {}",
            redact_endpoint(endpoint_str)
        );
        // Parse the endpoint and create client directly
        let (uri, user, password, database) = parse_neo4j_endpoint(endpoint_str)?;
        benchmark::neo4j_client::Neo4jClient::new(uri, user, password, database).await?
    } else {
        // Use local Neo4j instance (existing behavior)
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
        neo4j.client().await?
    };
    let (node_count, relation_count) = client.graph_size().await?;
    info!(
        "node count: {}, relation count: {}",
        format_number(node_count),
        format_number(relation_count)
    );
    if node_count != 0 || relation_count != 0 {
        if endpoint.is_some() {
            error!(
                "External Neo4j database is not empty, node count: {}, relation count: {}",
                node_count, relation_count
            );
            return Err(OtherError(
                "External database is not empty. Please clear the database manually before loading data.".to_string(),
            ));
        } else {
            error!(
                "graph is not empty, node count: {}, relation count: {}",
                node_count, relation_count
            );
            info!("For local Neo4j: database should be cleaned before loading");
            return Err(OtherError(
                "Database is not empty. Use --force to clear it first.".to_string(),
            ));
        }
    }
    let mut histogram = Histogram::new(7, 64)?;

    let mut index_stream = spec.init_index_iterator().await?;
    info!("importing indexes");
    client
        .execute_query_stream(&mut index_stream, &mut histogram)
        .await?;
    let data_stream = spec.init_data_iterator().await?;
    info!("importing data in batches of {}", batch_size);
    let start = Instant::now();
    let total_processed = client
        .execute_query_stream_batched(data_stream, batch_size, &mut histogram)
        .await?;
    info!("Processed {} data commands in batches", total_processed);
    let (node_count, relation_count) = client.graph_size().await?;
    info!(
        "{} nodes and {} relations were imported at {:?}",
        format_number(node_count),
        format_number(relation_count),
        start.elapsed()
    );

    // Only stop neo4j and dump if we're managing a local instance
    if endpoint.is_none() {
        // For local instances, we need to handle the neo4j instance
        // This is a limitation of the current design - we don't have access to the neo4j instance here
        info!("For local instances: stopping and dumping would happen here");
        // TODO: Refactor to properly handle local instance cleanup
    } else {
        info!("Using external endpoint, skipping Neo4j process management");
    }

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
    #[serde(default)]
    catalog: Vec<QueryCatalogEntry>,
}
async fn prepare_queries(
    dataset: Size,
    size: usize,
    file_name: String,
    write_ratio: f32,
) -> BenchmarkResult<()> {
    let start = Instant::now();
    let queries_repository =
        benchmark::queries_repository::UsersQueriesRepository::new(9998, 121716);
    let catalog = queries_repository.catalog();
    let metadata = PrepareQueriesMetadata {
        size,
        dataset,
        catalog,
    };
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

async fn run_memgraph(
    parallel: usize,
    file_name: String,
    mps: usize,
    simulate: Option<usize>,
    endpoint: Option<String>,
    results_dir: Option<String>,
) -> BenchmarkResult<()> {
    let queries_file = file_name.clone();
    let (queries_metadata, queries) = read_queries(file_name).await?;
    let number_of_queries = queries_metadata.size;

    let client = if let Some(ref endpoint_str) = endpoint {
        info!(
            "Using external Memgraph endpoint: {}",
            redact_endpoint(endpoint_str)
        );
        // Parse the endpoint and create client directly
        let (uri, user, password, _database) = parse_memgraph_endpoint(endpoint_str)?;
        benchmark::memgraph_client::MemgraphClient::new(uri, user, password).await?
    } else {
        // Use local Memgraph instance (existing behavior)
        let mut memgraph = benchmark::memgraph::Memgraph::default();
        // stop memgraph if it is running
        memgraph.stop(false).await?;
        let spec = Spec::new(Users, queries_metadata.dataset, Vendor::Memgraph);
        memgraph.restore_db(spec).await?;
        // start memgraph
        memgraph.start().await?;
        memgraph.client().await?
    };
    info!("client connected to memgraph");

    // Best-effort Memgraph storage/memory reporting (query-interface metric).
    client.collect_storage_info_metrics().await;

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

    // HDR histogram for accurate pXX latencies (microseconds)
    let latency_hist = Arc::new(tokio::sync::Mutex::new(histogram::Histogram::new(7, 64)?));

    // Per-query histograms for "single"-style percentiles (P10..P99)
    let per_query = Arc::new(PerQueryLatency::new(queries_metadata.catalog.clone())?);

    let started_at = SystemTime::now();
    let start = Instant::now();
    for spawn_id in 0..parallel {
        let handle = spawn_memgraph_worker(
            client.clone(),
            spawn_id,
            &rx,
            simulate,
            latency_hist.clone(),
            per_query.clone(),
        )
        .await?;
        workers_handles.push(handle);
    }
    let _ = scheduler_handle.await;
    drop(tx);

    for handle in workers_handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    let finished_at = SystemTime::now();

    info!(
        "running {} queries took {:?}",
        format_number(number_of_queries as u64),
        elapsed
    );

    // Export accurate pXX latency gauges (microseconds)
    {
        let hist = latency_hist.lock().await;
        MEMGRAPH_LATENCY_P50_US.set(percentile_us(&hist, 50.0) as i64);
        MEMGRAPH_LATENCY_P95_US.set(percentile_us(&hist, 95.0) as i64);
        MEMGRAPH_LATENCY_P99_US.set(percentile_us(&hist, 99.0) as i64);
    }

    // Export per-query percentiles.
    per_query.export_to_prometheus(Vendor::Memgraph);

    // Capture Memgraph memory numbers after the workload.
    client.collect_storage_info_metrics().await;

    write_run_results(
        results_dir,
        Vendor::Memgraph,
        queries_metadata.dataset,
        &queries_file,
        parallel,
        mps,
        simulate,
        &endpoint,
        number_of_queries,
        started_at,
        finished_at,
        elapsed,
    )
    .await?;

    // Only stop memgraph if we're managing a local instance
    if endpoint.is_none() {
        // For local instances, we need to properly stop memgraph
        // This is a limitation - we don't have access to the memgraph instance here
        // TODO: Refactor to properly handle local instance cleanup
        info!("For local Memgraph: stopping would happen here");
    } else {
        info!("Using external endpoint, skipping Memgraph process management");
    }

    Ok(())
}

async fn spawn_memgraph_worker(
    client: MemgraphClient,
    worker_id: usize,
    receiver: &Arc<Mutex<Receiver<Msg<PreparedQuery>>>>,
    simulate: Option<usize>,
    latency_hist: Arc<tokio::sync::Mutex<histogram::Histogram>>,
    per_query: Arc<PerQueryLatency>,
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
                            MEMGRAPH_SUCCESS_REQUESTS_DURATION_HISTOGRAM
                                .observe(duration.as_secs_f64());
                            // Accurate percentile source
                            {
                                let mut h = latency_hist.lock().await;
                                let _ = h.increment(duration.as_micros() as u64);
                            }
                            // Per-query latency tracking
                            per_query.record_us(
                                prepared_query.payload.q_id,
                                duration.as_micros() as u64,
                            );
                            counter += 1;
                            if counter % 1000 == 0 {
                                info!("worker {} processed {} queries", worker_id, counter);
                            }
                        }
                        Err(e) => {
                            MEMGRAPH_ERROR_REQUESTS_DURATION_HISTOGRAM
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

async fn dry_init_memgraph(
    size: Size,
    _batch_size: usize,
) -> BenchmarkResult<()> {
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Memgraph);
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

async fn init_memgraph(
    size: Size,
    force: bool,
    batch_size: usize,
    endpoint: Option<String>,
) -> BenchmarkResult<()> {
    let spec = Spec::new(benchmark::scenario::Name::Users, size, Vendor::Memgraph);

    let client = if let Some(ref endpoint_str) = endpoint {
        info!(
            "Using external Memgraph endpoint for data loading: {}",
            redact_endpoint(endpoint_str)
        );
        // Parse the endpoint and create client directly
        let (uri, user, password, _database) = parse_memgraph_endpoint(endpoint_str)?;
        let client = benchmark::memgraph_client::MemgraphClient::new(uri, user, password).await?;
        if force {
            client.clean_db().await?;
            info!("External Memgraph database cleared (--force)");
        }
        client
    } else {
        // Use local Memgraph instance (existing behavior)
        let mut memgraph = benchmark::memgraph::Memgraph::default();
        let _ = memgraph.stop(false).await?;
        let backup_path = format!("{}/memgraph.cypher", spec.backup_path());
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
            let out = memgraph.clean_db().await?;
            info!(
                "memgraph clean_db std_error returns {} ",
                String::from_utf8_lossy(&out.stderr)
            );
            info!(
                "memgraph clean_db std_out returns {} ",
                String::from_utf8_lossy(&out.stdout)
            );
        }

        memgraph.start().await?;
        memgraph.client().await?
    };
    let (node_count, relation_count) = client.graph_size().await?;
    info!(
        "node count: {}, relation count: {}",
        format_number(node_count),
        format_number(relation_count)
    );
    if node_count != 0 || relation_count != 0 {
        if endpoint.is_some() {
            error!(
                "External Memgraph database is not empty, node count: {}, relation count: {}",
                node_count, relation_count
            );
            return Err(OtherError(
                "External database is not empty. Please clear the database manually before loading data.".to_string(),
            ));
        } else {
            error!(
                "graph is not empty, node count: {}, relation count: {}",
                node_count, relation_count
            );
            info!("For local Memgraph: database should be cleaned before loading");
            return Err(OtherError(
                "Database is not empty. Use --force to clear it first.".to_string(),
            ));
        }
    }
    let mut histogram = Histogram::new(7, 64)?;

    let mut index_stream = spec.init_index_iterator().await?;
    info!("importing indexes");
    client
        .execute_query_stream(&mut index_stream, &mut histogram)
        .await?;
    let data_stream = spec.init_data_iterator().await?;
    info!("importing data in batches of {}", batch_size);
    let start = Instant::now();
    let total_processed = client
        .execute_query_stream_batched(data_stream, batch_size, &mut histogram)
        .await?;
    info!("Processed {} data commands in batches", total_processed);
    let (node_count, relation_count) = client.graph_size().await?;
    info!(
        "{} nodes and {} relations were imported at {:?}",
        format_number(node_count),
        format_number(relation_count),
        start.elapsed()
    );
    // Only stop memgraph and dump if we're managing a local instance
    if endpoint.is_none() {
        // For local instances, we need to handle the memgraph instance cleanup
        // This is a limitation of the current design - we don't have access to the memgraph instance here
        info!("For local Memgraph: stopping and dumping would happen here");
        // TODO: Refactor to properly handle local instance cleanup
    } else {
        info!("Using external endpoint, skipping Memgraph process management");
    }

    info!("---> histogram");
    show_historgam(histogram);

    info!("---> Done");
    Ok(())
}
