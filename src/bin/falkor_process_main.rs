use benchmark::error::BenchmarkResult;
use benchmark::falkor::falkor_process::FalkorProcess;
use std::time::Duration;
use tracing::info;

#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    tracing_subscriber::fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .init();

    let _falkor_process = FalkorProcess::new().await?;

    info!("waiting 30 seconds before dropping the falkor process");
    // Run the monitor in a separate task
    tokio::time::sleep(Duration::from_secs(30)).await;

    info!("Dropping the falkor process");
    // drop(falkor_process);

    Ok(())
}
