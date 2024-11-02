use benchmark::error::BenchmarkResult;
use benchmark::neo4j::Neo4j;
use benchmark::neo4j_client::Neo4jClient;
use criterion::measurement::WallTime;
use criterion::{criterion_group, criterion_main, BenchmarkGroup, Criterion, SamplingMode};
use futures::StreamExt;
use tokio::runtime::Runtime;
use tokio::time::Instant;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

// async fn fibonacci(n: u64) -> u64 {
//     n
// }

fn benchmark(c: &mut Criterion) {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let rt = Runtime::new().unwrap();
    let mut group: BenchmarkGroup<WallTime> = c.benchmark_group("neo4j_operations");
    // group.sample_size(30);
    // group.sampling_mode(SamplingMode::Flat);
    group.bench_function("Neo4j", |b| {
        info!("Starting Iteration");
        let _ = rt.block_on(async { Neo4j::new().start().await.unwrap() });
        b.to_async(&rt).iter_custom(|iters| async move {
            info!("running iteration of {} iters", iters);

            let client = Neo4j::new().client().await.unwrap();
            let start = Instant::now();
            for _ in 0..iters {
                let _results_count = perform_neo4j_operation(&client).await.unwrap();
            }
            let duration = start.elapsed();
            info!(
                "Finish iteration of {} in {}",
                iters,
                duration.as_secs_f64()
            );
            duration
        });
        rt.block_on(async { Neo4j::new().stop().await.unwrap() });
    });
    group.finish();
    // neo4j.stop().await.unwrap();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
    .measurement_time(std::time::Duration::from_secs(10 * 60));
    targets = benchmark
}

criterion_main!(benches);

async fn perform_neo4j_operation(client: &Neo4jClient) -> BenchmarkResult<u64> {
    // Your async benchmarked operation here
    let query = r#"CREATE
  (n1:Person {name: 'Alice'}),
  (n2:Person {name: 'Bob'}),
  (n3:Person {name: 'Charlie'}),
  (n4:Person {name: 'David'}),
  (n5:Person {name: 'Eve'})
RETURN n1, n2, n3, n4, n5"#
        .to_string();
    let mut stream = client.execute_query(query).await?;
    let mut count = 0;
    while let Some(Ok(_row)) = stream.next().await {
        // info!("Row: {:?}", row);
        count += 1;
    }
    Ok(count)

    // fibonacci(20).await
}
