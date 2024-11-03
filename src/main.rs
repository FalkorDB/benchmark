mod error;
mod line_stream;
mod neo4j;
mod neo4j_client;
mod neo4j_process;
pub mod scenario;
mod utils;
mod foo;

use crate::error::BenchmarkResult;
use crate::scenario::{Size, Vendor};
use futures::{StreamExt, TryStreamExt};
use histogram::Histogram;
use scenario::Spec;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{error, info, trace, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(Level::INFO)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    let spec = Spec::new(scenario::Name::Users, Size::Small, Vendor::Neo4j);
    let neo4j = neo4j::Neo4j::new();


    neo4j.start().await?;

    let client = neo4j.client().await?;

    let mut histogram = Histogram::new(7, 64).unwrap();

    let mut lines = spec.init_data_iterator().await?;
    let mut counter = 0;
    let start = Instant::now();
    for line_or_error in lines {
        counter += 1;
        if counter % 1000 == 0 {
            info!("---> done with {} queries at {:?}", counter, start.elapsed());
        }
        match line_or_error {
            Ok(line) => {
                // info!("line: {}", line);
                if line.clone().trim() == ";" {
                    continue;
                }
                trace!("executing {}", line);
                let start = Instant::now();
                let mut stream = client.execute_query_str(line.as_str()).await?;
                while let Some(Ok(row)) = stream.next().await {
                    trace!("Row: {:?}", row);
                }
                let duration = start.elapsed();
                // info!("Duration: {:?}, counter {}", duration, counter);
                histogram.increment(duration.as_micros() as u64).unwrap();
            }
            Err(error) => {
                error!("error: {:?}", error);
            }
        }
    }

    info!("---> done with {} queries at {:?}", counter, start.elapsed());
    neo4j.clone().stop().await?;
    neo4j.dump(spec.clone()).await?;
    info!("---> historam");
    let p50 = histogram
        .percentile(50.0)
        .map(|r| r.map(|b| Duration::from_micros(b.end())));
    let p90 = histogram
        .percentile(90.0)
        .map(|r| r.map(|b| Duration::from_micros(b.end())));
    let p99 = histogram
        .percentile(99.0)
        .map(|r| r.map(|b| Duration::from_micros(b.end())));
    info!("p50: {:?}", p50);
    info!("p90: {:?}", p90);
    info!("p99: {:?}", p99);

    info!("---> Done");
    Ok(())
}
