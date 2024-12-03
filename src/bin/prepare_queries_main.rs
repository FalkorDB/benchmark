use benchmark::error::BenchmarkResult;
use benchmark::queries_repository::PreparedQuery;
use benchmark::utils::read_lines;
use serde::Serialize;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::time::Instant;
use tokio_stream::StreamExt;
use tracing::{error, trace};

#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    tracing_subscriber::fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .init();

    write_queries().await?;
    let start = Instant::now();
    read_queries().await?;
    let duration = start.elapsed();
    println!("read_queries took: {:?}", duration);
    Ok(())
}

async fn read_queries() -> BenchmarkResult<()> {
    let mut lines = read_lines("output.txt").await?;
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => {
                if let Ok(query) = serde_json::from_str::<PreparedQuery>(&line) {
                    trace!("Query: {:?}", query);
                } else {
                    error!("Failed to deserialize query");
                }
            }
            Err(e) => error!("Failed to read line: {}", e),
        }
    }

    Ok(())
}

pub async fn write_iterator_to_file<F, S, I>(
    file_name: F,
    iterator: I,
) -> BenchmarkResult<()>
where
    F: AsRef<str>,
    S: Serialize,
    I: Iterator<Item = S>,
{
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(file_name.as_ref())
        .await?;

    let mut writer = BufWriter::new(file);

    for item in iterator {
        if let Ok(json) = serde_json::to_string(&item) {
            if let Err(e) = writer.write_all(json.as_bytes()).await {
                error!("Failed to write to file: {}", e);
            }
            if let Err(e) = writer.write_all(b"\n").await {
                error!("Failed to write newline: {}", e);
            }
        } else {
            error!("Failed to serialize query");
        }
    }
    writer.flush().await?;
    Ok(())
}

async fn write_queries() -> BenchmarkResult<()> {
    let queries_repository =
        benchmark::queries_repository::UsersQueriesRepository::new(9998, 121716);
    let queries_iter = queries_repository.random_queries(1000000);

    write_iterator_to_file("output.txt", queries_iter).await?;
    Ok(())
}
