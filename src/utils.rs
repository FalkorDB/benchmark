use crate::error::BenchmarkError::{FailedToDownloadFileError, FailedToSpawnNeo4jError};
use crate::error::BenchmarkResult;
use futures::stream::{self, Stream};
use std::io::Cursor;
// use futures::StreamExt;
use std::pin::Pin;
use std::process::Output;
use tokio::fs;
use tokio::fs::{copy, File};
use tokio::io::{self, AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, Lines};
use tokio::process::Command;
use tracing::info;


pub(crate) async fn spawn_command(
    command: &str,
    args: &[&str],
) -> BenchmarkResult<Output> {
    let output = Command::new(command).args(args).output().await?;

    if !output.status.success() {
        let args_str = args.join(" ");
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(FailedToSpawnNeo4jError(
            std::io::Error::new(std::io::ErrorKind::Other, "Process failed"),
            format!(
                "Failed to spawn Neo4j process, path: {} with args: {}, Error: {}",
                command, args_str, stderr
            ),
        ));
    }
    Ok(output)
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
    info!("Downloading to file {} from {}", file_name , url);
    // Send a GET request to the specified URL
    let client = reqwest::Client::builder().gzip(true).build()?;
    let response = client.get(url).send().await?;

    // Ensure the response is successful
    if response.status().is_success() {
        // Create a new file to write the downloaded content to
        let mut file = File::create(file_name).await?;

        // Copy the response body into the file
        let content = response.bytes().await?;
        file.write_all(&content).await?;
        // copy(&mut content.as_ref(), &mut file).await?;

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


