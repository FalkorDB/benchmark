#![allow(unused_variables, unused_imports, dead_code)]

mod error;
mod line_stream;
mod neo4j_client;
mod neo4j_process;
pub mod scenario;

use crate::error::BenchmarkResult;
use crate::scenario::{Size, Vendor};
use futures::{StreamExt, TryStreamExt};
use histogram::Histogram;
use scenario::Spec;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{info, trace, Level};
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

    let neo4j_path = String::from("/Users/barak/dev/benchmark/downloads/neo4j_local/bin/neo4j");
    let neo4j = neo4j_process::Neo4jProcess::new(neo4j_path);
    neo4j.start().await?;

    let uri = "127.0.0.1:7687";
    let user = "neo4j";
    let pass = "h6u4krd10";

    let client =
        neo4j_client::Neo4jClient::new(uri.to_string(), user.to_string(), pass.to_string()).await?;

    let query1 = r#"CREATE
  (n1:Person {name: 'Alice'}),
  (n2:Person {name: 'Bob'}),
  (n3:Person {name: 'Charlie'}),
  (n4:Person {name: 'David'}),
  (n5:Person {name: 'Eve'})
RETURN n1, n2, n3, n4, n5"#;

    let query2 = r#"WITH range(1, 10000) AS ids
UNWIND ids AS id
CREATE (n:Dog {id: id})
    RETURN n"#;

    let query3 = r#"MATCH (n:Dog)
RETURN n"#;

    let query4 = r#"MATCH (n:Node)
WHERE n.id % 2 = 1
RETURN sum(n.id) AS totalOddIdSum"#;

    let mut histogram = Histogram::new(7, 64).unwrap();
    for iteration in 0..100 {
        let start = Instant::now();
        let mut stream = client.execute_query_str(query4).await?;
        let mut counter = 0;
        while let Some(Ok(row)) = stream.next().await {
            info!("Row: {:?}", row);
            counter += 1;
        }
        let duration = start.elapsed();
        histogram.increment(duration.as_micros() as u64).unwrap();
        info!(
            "Iteration {}, Duration: {:?}, counter {}",
            iteration, duration, counter
        );
    }
    let p50 = histogram.percentile(90.0).unwrap().unwrap().end();
    let p90 = histogram.percentile(90.0).unwrap().unwrap().end();
    let p99 = histogram.percentile(99.0).unwrap().unwrap().end();
    info!("p50: {:?}", Duration::from_micros(p50));
    info!("p90: {:?}", Duration::from_micros(p90));
    info!("p99: {:?}", Duration::from_micros(p99));

    neo4j.stop().await?;

    let spec = scenario::Spec::new(scenario::Name::Pokec, scenario::Size::Small, Vendor::Neo4j);
    let mut line_stream = spec.stream_data().await?;
    while let Some(line) = line_stream.try_next().await? {
        info!("{}", line);
    }
    info!("---> Done");
    Ok(())
}
