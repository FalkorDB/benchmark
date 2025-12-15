use crate::error::BenchmarkError::{
    FailedToDownloadFileError, FailedToSpawnProcessError, OtherError, ProcessNofFoundError,
};
use crate::error::{BenchmarkError, BenchmarkResult};
use futures::stream::Stream;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::env;
use std::path::Path;
use std::process::Output;
use std::str;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::sleep;
use tokio::{fs, io};
use tokio_stream::StreamExt;
use tracing::{error, info, trace};

pub async fn spawn_command(
    command: &str,
    args: &[&str],
) -> BenchmarkResult<Output> {
    info!("Spawning command: {} {}", command, args.join(" "));
    let output = Command::new(command).args(args).output().await?;

    if !output.status.success() {
        let args_str = args.join(" ");
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(FailedToSpawnProcessError(
            io::Error::new(std::io::ErrorKind::Other, "Process failed"),
            format!(
                "Failed to spawn Neo4j process, path: {} with args: {}, Error: {}",
                command, args_str, stderr
            ),
        ));
    }
    Ok(output)
}

pub async fn file_exists(file_path: &str) -> bool {
    fs::metadata(file_path).await.is_ok()
}
pub async fn delete_file(file_path: &str) -> BenchmarkResult<()> {
    if file_exists(file_path).await {
        info!("Deleting file: {}", file_path);
        fs::remove_file(file_path).await?;
    }
    Ok(())
}

pub fn falkor_shared_lib_path() -> BenchmarkResult<String> {
    if let Ok(path) = env::current_dir() {
        Ok(format!("{}/falkordb.so", path.display()))
    } else {
        Err(OtherError("Failed to get current directory".to_string()))
    }
}
pub fn falkor_logs_path() -> BenchmarkResult<String> {
    if let Ok(path) = env::current_dir() {
        Ok(format!("{}/falkordb.log", path.display()))
    } else {
        Err(OtherError("Failed to get current directory".to_string()))
    }
}
pub fn get_falkor_log_path() -> BenchmarkResult<String> {
    let default_falkor_log_path = falkor_logs_path()?;
    let falkor_log_path =
        env::var("FALKOR_LOG_PATH").unwrap_or_else(|_| default_falkor_log_path.clone());
    Ok(falkor_log_path)
}

pub async fn create_directory_if_not_exists(dir_path: &str) -> BenchmarkResult<()> {
    // Check if the directory exists
    if fs::metadata(dir_path).await.is_err() {
        // If it doesn't exist, create the directory
        fs::create_dir_all(dir_path).await?;
    }
    Ok(())
}

pub fn url_file_name(url: &str) -> String {
    let url_parts: Vec<&str> = url.split('/').collect();
    url_parts[url_parts.len() - 1].to_string()
}
pub async fn download_file(
    url: &str,
    file_name: &str,
) -> BenchmarkResult<()> {
    info!("Downloading to file {} from {}", file_name, url);
    // Send a GET request to the specified URL
    let client = reqwest::Client::builder().gzip(true).build()?;
    let response = client.get(url).send().await?;

    // Ensure the response is successful
    if response.status().is_success() {
        // Create a new file to write the downloaded content to
        let mut file = File::create(file_name).await?;
        let bytes = response.bytes().await?;
        file.write_all(&bytes).await?;
        file.flush().await?;

        Ok(())
    } else {
        Err(FailedToDownloadFileError(
            format!(
                "Failed to download a file {}, http status: {}, request: {}",
                file_name,
                response.status(),
                url
            )
            .to_string(),
        ))
    }
}

pub async fn read_lines<P>(
    filename: P
) -> BenchmarkResult<impl Stream<Item = Result<String, io::Error>>>
where
    P: AsRef<Path>,
{
    // Open the file asynchronously
    let file = File::open(filename).await?;

    // Create a buffered reader
    let reader = BufReader::new(file);

    let stream = tokio_stream::wrappers::LinesStream::new(reader.lines()).filter_map(|res| {
        match res {
            Ok(line) => {
                // filter out empty lines or lines that contain only a semicolon
                let trimmed_line = line.trim();
                if trimmed_line.is_empty() || trimmed_line == ";" {
                    None
                } else {
                    Some(Ok(line))
                }
            }
            Err(e) => Some(Err(e)), // Propagate errors
        }
    });

    Ok(stream)
}

pub async fn kill_process(pid: u32) -> BenchmarkResult<()> {
    let pid = Pid::from_raw(pid as i32);
    match kill(pid, Signal::SIGKILL) {
        Ok(_) => Ok(()),
        Err(nix::Error::ESRCH) => Err(OtherError(format!("No process with pid {} found", pid))),
        Err(e) => Err(OtherError(format!("Failed to kill process {}: {}", pid, e))),
    }
}

pub async fn get_command_pid(cmd: impl AsRef<str>) -> BenchmarkResult<u32> {
    let cmd = cmd.as_ref();
    let output = Command::new("ps")
        .args(["-eo", "pid,command,stat"])
        .output()
        .await
        .map_err(BenchmarkError::IoError)?;

    if output.status.success() {
        let stdout = str::from_utf8(&output.stdout)
            .map_err(|e| OtherError(format!("UTF-8 conversion error: {}", e)))?;

        for (index, line) in stdout.lines().enumerate() {
            if index == 0 || line.contains("grep") {
                continue;
            }
            if line.contains(cmd) {
                if let [pid, command, stat, ..] =
                    line.split_whitespace().collect::<Vec<_>>().as_slice()
                {
                    if command.contains(cmd) {
                        if stat.starts_with("Z") || stat.contains("<defunct>") {
                            continue;
                        }
                        return pid
                            .parse::<u32>()
                            .map_err(|e| OtherError(format!("Failed to parse PID: {}", e)));
                    }
                }
            }
        }
        Err(ProcessNofFoundError(cmd.to_string()))
    } else {
        error!(
            "ps command failed with exit code: {:?}",
            output.status.code()
        );
        Err(OtherError(format!(
            "ps command failed with exit code: {:?}",
            output.status.code()
        )))
    }
}

pub async fn ping_redis() -> BenchmarkResult<()> {
    let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    let mut con = client.get_multiplexed_async_connection().await?;

    let timeout_duration = Duration::from_secs(10);

    let result = tokio::time::timeout(timeout_duration, async {
        let pong: String = redis::cmd("PING").query_async(&mut con).await?;
        trace!("Redis ping response: {}", pong);
        if pong == "PONG" {
            Ok(())
        } else {
            Err(OtherError(format!(
                "Unexpected response from Redis: {}",
                pong
            )))
        }
    })
    .await;

    result.unwrap_or_else(|_| Err(OtherError("Ping operation timed out".to_string())))
}

pub async fn wait_for_redis_ready(
    max_attempts: u32,
    delay: Duration,
) -> BenchmarkResult<()> {
    for attempt in 1..=max_attempts {
        match ping_redis().await {
            Ok(_) => {
                trace!("redis is ready after {} attempt(s)", attempt);
                return Ok(());
            }
            Err(e) => {
                if attempt < max_attempts {
                    trace!(
                        "Attempt {} failed to connect to Redis: {}. Retrying...",
                        attempt,
                        e
                    );
                    sleep(delay).await;
                } else {
                    error!("Failed to connect to Redis after {} attempts", max_attempts);
                    return Err(BenchmarkError::OtherError(format!(
                        "Redis not ready after {} attempts",
                        max_attempts
                    )));
                }
            }
        }
    }
    unreachable!()
}

pub async fn redis_save() -> BenchmarkResult<()> {
    let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    let mut con = client.get_multiplexed_async_connection().await?;

    // Set a timeout of 30 seconds
    let timeout_duration = Duration::from_secs(30);

    // Use tokio's timeout function
    let result = tokio::time::timeout(timeout_duration, async {
        let pong: String = redis::cmd("SAVE").query_async(&mut con).await?;
        trace!("Redis SAVE response: {}", pong);
        if pong == "OK" {
            Ok(())
        } else {
            Err(OtherError(format!(
                "Unexpected response from Redis: {}",
                pong
            )))
        }
    })
    .await;

    result.unwrap_or_else(|_| Err(OtherError("SAVE operation timed out".to_string())))
}

pub async fn redis_shutdown() -> BenchmarkResult<()> {
    info!("Shutting down Redis");

    // Set a timeout of 20 seconds
    let timeout_duration = Duration::from_secs(20);

    // Attempt to open the Redis client and connection with a timeout
    let result = tokio::time::timeout(timeout_duration, async {
        let client = redis::Client::open("redis://127.0.0.1:6379/")?;
        let mut con = client.get_multiplexed_async_connection().await?;

        // Send the SHUTDOWN command
        let response: String = redis::cmd("SHUTDOWN").query_async(&mut con).await?;
        info!("Redis shutdown command response: {}", response);

        Ok::<(), BenchmarkError>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {
            info!("Redis shutdown command executed successfully.");
            Ok(())
        }
        Ok(Err(_)) => Ok(()),
        Err(e) => {
            error!(
                "Failed to shutdown Redis within {} seconds: {}. Attempting to forcefully kill the process.",
                timeout_duration.as_secs(),
                e
            );
            let redis_pid = get_command_pid("redis-server").await?;
            error!("Killing Redis process with PID: {}", redis_pid);
            kill_process(redis_pid).await?;
            Ok::<(), BenchmarkError>(())
        }
    }
}
pub async fn write_to_file(
    file_path: &str,
    content: &str,
) -> BenchmarkResult<()> {
    let mut file = File::create(file_path).await?;
    file.write_all(content.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}
pub fn format_number(num: u64) -> String {
    let mut s = String::new();
    let num_str = num.to_string();
    let a = num_str.chars().rev().enumerate();
    for (i, c) in a {
        if i != 0 && i % 3 == 0 {
            s.insert(0, ',');
        }
        s.insert(0, c);
    }
    s
}

/// Collects items from a stream into batches of specified size
pub async fn collect_batches<T, S, E>(
    mut stream: S,
    batch_size: usize,
) -> Vec<Vec<T>>
where
    S: StreamExt<Item = Result<T, E>> + Unpin,
    E: std::fmt::Debug,
{
    let mut batches = Vec::new();
    let mut current_batch = Vec::with_capacity(batch_size);

    while let Some(item_result) = stream.next().await {
        match item_result {
            Ok(item) => {
                current_batch.push(item);
                if current_batch.len() >= batch_size {
                    batches.push(current_batch);
                    current_batch = Vec::with_capacity(batch_size);
                }
            }
            Err(e) => {
                error!("Error processing stream item: {:?}", e);
            }
        }
    }

    // Add remaining items if any
    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}

/// Process items from a stream in batches with a callback function
pub async fn process_stream_in_batches<T, S, E, F, Fut>(
    mut stream: S,
    batch_size: usize,
    mut process_batch: F,
) -> BenchmarkResult<usize>
where
    S: StreamExt<Item = Result<T, E>> + Unpin,
    E: std::fmt::Debug,
    F: FnMut(Vec<T>) -> Fut,
    Fut: std::future::Future<Output = BenchmarkResult<()>>,
{
    let mut current_batch = Vec::with_capacity(batch_size);
    let mut total_processed = 0;
    let mut batch_count = 0;
    let start_time = tokio::time::Instant::now();
    let mut last_progress_report = start_time;
    const PROGRESS_INTERVAL_SECS: u64 = 5;

    while let Some(item_result) = stream.next().await {
        match item_result {
            Ok(item) => {
                current_batch.push(item);
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

                    process_batch(current_batch).await?;
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
        process_batch(current_batch).await?;
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
    Ok(total_processed)
}
