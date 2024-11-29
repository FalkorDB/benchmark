use benchmark::{error::BenchmarkResult, falkor_process::FalkorProcess};
use tracing::info;
#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    tracing_subscriber::fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .init();
    let _falkor_process = FalkorProcess::new().await?;
    info!("Falkor process started waiting 30 seconds");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    // info!("killing falkor process");
    // falkor_process.kill().await?;
    Ok(())
}
