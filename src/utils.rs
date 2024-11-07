use crate::error::BenchmarkError::{
    FailedToDownloadFileError, FailedToSpawnProcessError, OtherError,
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

pub(crate) async fn spawn_command(
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

pub(crate) async fn file_exists(file_path: &str) -> bool {
    fs::metadata(file_path).await.is_ok()
}
pub(crate) async fn delete_file(file_path: &str) -> BenchmarkResult<()> {
    if file_exists(file_path).await {
        info!("Deleting file: {}", file_path);
        fs::remove_file(file_path).await?;
    }
    Ok(())
}

pub(crate) fn falkor_shared_lib_path() -> BenchmarkResult<String> {
    if let Ok(path) = env::current_dir() {
        Ok(format!("{}/falkordb.so", path.display()))
    } else {
        Err(OtherError("Failed to get current directory".to_string()))
    }
}

pub(crate) async fn create_directory_if_not_exists(dir_path: &str) -> BenchmarkResult<()> {
    // Check if the directory exists
    if fs::metadata(dir_path).await.is_err() {
        // If it doesn't exist, create the directory
        fs::create_dir_all(dir_path).await?;
    }
    Ok(())
}

pub(crate) fn url_file_name(url: &str) -> String {
    let url_parts: Vec<&str> = url.split('/').collect();
    url_parts[url_parts.len() - 1].to_string()
}
pub(crate) async fn download_file(
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

pub(crate) async fn read_lines<P>(
    filename: P
) -> BenchmarkResult<impl Stream<Item = Result<String, std::io::Error>>>
where
    P: AsRef<Path>,
{
    // Open the file asynchronously
    let file = File::open(filename).await?;

    // Create a buffered reader
    let reader = BufReader::new(file);

    let stream = tokio_stream::wrappers::LinesStream::new(reader.lines())
        .map(|res| res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));

    Ok(stream)
}

pub(crate) async fn kill_process(pid: u32) -> BenchmarkResult<()> {
    let pid = Pid::from_raw(pid as i32);
    match kill(pid, Signal::SIGKILL) {
        Ok(_) => Ok(()),
        Err(nix::Error::ESRCH) => Err(OtherError(format!("No process with pid {} found", pid))),
        Err(e) => Err(OtherError(format!("Failed to kill process {}: {}", pid, e))),
    }
}

pub(crate) async fn get_command_pid(cmd: impl AsRef<str>) -> BenchmarkResult<Option<u32>> {
    let cmd = cmd.as_ref();
    let output = Command::new("ps")
        .args(&["aux"])
        .output()
        .await
        .map_err(|e| BenchmarkError::IoError(e))?;

    if output.status.success() {
        let stdout = str::from_utf8(&output.stdout)
            .map_err(|e| OtherError(format!("UTF-8 conversion error: {}", e)))?;

        for line in stdout.lines() {
            if line.contains(cmd) && !line.contains("grep") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() > 1 {
                    return parts[1]
                        .parse::<u32>()
                        .map(Some)
                        .map_err(|e| OtherError(format!("Failed to parse PID: {}", e)));
                }
            }
        }
        Ok(None)
    } else {
        Err(OtherError(format!(
            "ps command failed with exit code: {:?}",
            output.status.code()
        )))
    }
}

pub(crate) async fn ping_redis() -> BenchmarkResult<()> {
    let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    let mut con = client.get_multiplexed_async_connection().await?;

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
}

pub(crate) async fn wait_for_redis_ready(
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

pub(crate) async fn redis_save() -> BenchmarkResult<()> {
    let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    let mut con = client.get_multiplexed_async_connection().await?;

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
}
pub(crate) async fn write_to_file(
    file_path: &str,
    content: &str,
) -> BenchmarkResult<()> {
    let mut file = File::create(file_path).await?;
    file.write_all(content.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}
pub(crate) fn format_number(num: u64) -> String {
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
