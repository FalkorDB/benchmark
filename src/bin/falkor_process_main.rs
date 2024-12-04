use benchmark::{error::BenchmarkResult, falkor_process::FalkorProcess};
use falkordb::{FalkorClientBuilder, FalkorConnectionInfo};
use redis::RedisResult;
use tracing::info;
#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    tracing_subscriber::fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .init();

    // quit old redis if it's running
    if let Ok(shutdown_result) = shutdown_redis().await {
        info!("Redis shutdown result: {}", shutdown_result);
        info!("Falkor process started waiting 3 seconds");
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    }

    let mut falkor_process = FalkorProcess::new().await?;

    info!("Falkor process started waiting 3 seconds");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let connection_info: FalkorConnectionInfo = "falkor://127.0.0.1:6379"
        .try_into()
        .expect("Invalid connection info");

    let client = FalkorClientBuilder::new_async()
        .with_connection_info(connection_info)
        .with_num_connections(nonzero::nonzero!(1u8))
        .build()
        .await
        .expect("Failed to build client");

    let mut graph = client.select_graph("falkor");

    info!("Checking redis connection");
    // let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    // let mut con = client.get_multiplexed_async_connection().await?;
    for _ in 0..300000 {
        // match redis::cmd("PING").query_async::<String>(&mut con).await {
        //     Ok(pong) => {
        //         info!("Redis ping response: {}", pong);
        //     }
        //     Err(e) => {
        //         if e.kind() == redis::ErrorKind::IoError {
        //             info!("Redis connection error: {}", e);
        //             con = client.get_multiplexed_async_connection().await?;
        //         }
        //         info!("Redis error: {}, kind: {:?}", e, e.kind());
        //     }
        // }
        let nodes = graph
            .query("MATCH(n) RETURN count(n) as nodeCount")
            .with_timeout(5000)
            .execute()
            .await;

        match nodes {
            Ok(mut rs) => {
                while let Some(node) = rs.data.next() {
                    info!("Node: {:?}", node);
                }
            }
            Err(e) => {
                info!("Error: {:?}", e);
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }

    info!("killing falkor process");
    falkor_process.terminate().await?;
    Ok(())
}

async fn shutdown_redis() -> RedisResult<String> {
    let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    let mut con = client.get_multiplexed_async_connection().await?;
    let result: String = redis::cmd("SHUTDOWN").query_async(&mut con).await?;
    Ok(result)
}
