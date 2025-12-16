use lazy_static::lazy_static;
use prometheus::register_counter_vec;
use prometheus::register_histogram;
use prometheus::register_int_counter;
use prometheus::register_int_gauge;
use prometheus::register_int_gauge_vec;
use prometheus::CounterVec;
use prometheus::Histogram;
use prometheus::IntCounter;
use prometheus::IntGauge;
use prometheus::IntGaugeVec;

pub mod cli;
pub mod error;
pub mod falkor;
pub mod memgraph;
pub mod memgraph_client;
pub mod neo4j;
pub mod neo4j_client;
pub mod process_monitor;
pub mod prometheus_endpoint;
pub mod prometheus_metrics;
pub mod queries_repository;
pub mod query;
pub mod scenario;
pub mod scheduler;
pub mod utils;

pub(crate) const REDIS_DATA_DIR: &str = "./redis-data";

lazy_static! {
    pub static ref OPERATION_COUNTER: CounterVec = register_counter_vec!(
        "operations_total",
        "Total number of operations processed",
        &[
            "vendor",
            "spawn_id",
            "type",
            "name",
            "dataset",
            "dataset_size"
        ]
    )
    .unwrap();
    pub static ref OPERATION_ERROR_COUNTER: CounterVec = register_counter_vec!(
        "operations_error_total",
        "Total number of operations failed",
        &[
            "vendor",
            "spawn_id",
            "type",
            "name",
            "dataset",
            "dataset_size"
        ]
    )
    .unwrap();
    pub static ref FALKOR_RESTART_COUNTER: IntCounter = register_int_counter!(
        "falkordb_restarts_total",
        "Total number of restart for falkordb server",
    )
    .unwrap();
    pub static ref FALKOR_RUNNING_REQUESTS_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_running_requests",
        "The number of request that run now by the falkordb server",
    )
    .unwrap();
    pub static ref FALKOR_WAITING_REQUESTS_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_waiting_requests",
        "The number of request that waiting to run by the falkordb server",
    )
    .unwrap();
    pub static ref FALKOR_NODES_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_nodes_total",
        "Total number of nodes in falkordb graph",
    )
    .unwrap();
    pub static ref FALKOR_RELATIONSHIPS_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_relationships_total",
        "Total number of relationships in falkordb graph",
    )
    .unwrap();
    pub static ref FALKOR_SUCCESS_REQUESTS_DURATION_HISTOGRAM: Histogram = register_histogram!(
        "falkordb_response_time_success_histogram",
        "Response time histogram of the successful requests",
        vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,]
    )
    .unwrap();
    pub static ref FALKOR_ERROR_REQUESTS_DURATION_HISTOGRAM: Histogram = register_histogram!(
        "falkordb_response_time_error_histogram",
        "Response time histogram of the error requests",
        vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,]
    )
    .unwrap();
    pub static ref FALKOR_MSG_DEADLINE_OFFSET_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_msg_deadline_offset",
        "offset of the message from the deadline",
    )
    .unwrap();
    pub static ref NEO4J_SUCCESS_REQUESTS_DURATION_HISTOGRAM: Histogram = register_histogram!(
        "neo4j_response_time_success_histogram",
        "Response time histogram of the successful requests",
        vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,]
    )
    .unwrap();
    pub static ref NEO4J_ERROR_REQUESTS_DURATION_HISTOGRAM: Histogram = register_histogram!(
        "neo4j_response_time_error_histogram",
        "Response time histogram of the error requests",
        vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,]
    )
    .unwrap();
    pub static ref NEO4J_MSG_DEADLINE_OFFSET_GAUGE: IntGauge = register_int_gauge!(
        "neo4j_msg_deadline_offset",
        "offset of the message from the deadline",
    )
    .unwrap();
    pub static ref CPU_USAGE_GAUGE: IntGauge =
        register_int_gauge!("cpu_usage", "CPU usage percentage").unwrap();
    pub static ref MEM_USAGE_GAUGE: IntGauge =
        register_int_gauge!("memory_usage", "Memory usage in bytes").unwrap();
    pub static ref FALKOR_CPU_USAGE_GAUGE: IntGauge = register_int_gauge!(
        "falkor_cpu_usage",
        "CPU usage percentage for the falkordb process"
    )
    .unwrap();
    pub static ref FALKOR_MEM_USAGE_GAUGE: IntGauge = register_int_gauge!(
        "falkor_memory_usage",
        "Memory usage in bytes for the falkordb process"
    )
    .unwrap();
    pub static ref NEO4J_CPU_USAGE_GAUGE: IntGauge = register_int_gauge!(
        "neo4j_cpu_usage",
        "CPU usage percentage for the neo4j process"
    )
    .unwrap();
    pub static ref NEO4J_MEM_USAGE_GAUGE: IntGauge = register_int_gauge!(
        "neo4j_memory_usage",
        "Memory usage in bytes for the neo4j process"
    )
    .unwrap();

    // Neo4j JVM memory (via JMX / dbms.queryJmx). Useful for external endpoints where RSS isn't accessible.
    pub static ref NEO4J_JVM_HEAP_USED_BYTES: IntGauge = register_int_gauge!(
        "neo4j_jvm_heap_used_bytes",
        "JVM heap used bytes reported by java.lang:type=Memory"
    )
    .unwrap();
    pub static ref NEO4J_JVM_NONHEAP_USED_BYTES: IntGauge = register_int_gauge!(
        "neo4j_jvm_nonheap_used_bytes",
        "JVM non-heap used bytes reported by java.lang:type=Memory"
    )
    .unwrap();

    // Neo4j dataset footprint estimate (bytes) based on Neo4j sizing guidelines.
    // This is intended as a fallback when store sizing and JMX are unavailable (e.g. external endpoints).
    pub static ref NEO4J_BASE_DATASET_ESTIMATE_BYTES: IntGauge = register_int_gauge!(
        "neo4j_base_dataset_estimate_bytes",
        "Estimated base dataset size in bytes (Neo4j sizing guideline approximation)"
    )
    .unwrap();

    // Convenience metric: same estimate in MiB (rounded).
    pub static ref NEO4J_BASE_DATASET_ESTIMATE_MIB: IntGauge = register_int_gauge!(
        "neo4j_base_dataset_estimate_mib",
        "Estimated base dataset size in MiB (Neo4j sizing guideline approximation)"
    )
    .unwrap();

    // Neo4j store size (data + indexes) in bytes.
    //
    // Local runs: computed from filesystem sizes under the Neo4j DB directory.
    // External endpoints: best-effort via JMX exposed through Cypher (`dbms.queryJmx`).
    //
    // This is intended as a proxy for the dataset footprint that Neo4j would ideally keep hot in page cache.
    pub static ref NEO4J_STORE_SIZE_BYTES: IntGauge = register_int_gauge!(
        "neo4j_store_size_bytes",
        "Approximate size in bytes of Neo4j store files and schema/native indexes"
    )
    .unwrap();

    pub static ref NEO4J_STORE_SIZE_COLLECT_FAILURES_TOTAL: IntCounter = register_int_counter!(
        "neo4j_store_size_collect_failures_total",
        "Number of failures while trying to collect Neo4j store-size via Cypher/JMX"
    )
    .unwrap();
    pub static ref MEMGRAPH_SUCCESS_REQUESTS_DURATION_HISTOGRAM: Histogram = register_histogram!(
        "memgraph_response_time_success_histogram",
        "Response time histogram of the successful requests",
        vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,]
    )
    .unwrap();
    pub static ref MEMGRAPH_ERROR_REQUESTS_DURATION_HISTOGRAM: Histogram = register_histogram!(
        "memgraph_response_time_error_histogram",
        "Response time histogram of the error requests",
        vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,]
    )
    .unwrap();
    pub static ref MEMGRAPH_MSG_DEADLINE_OFFSET_GAUGE: IntGauge = register_int_gauge!(
        "memgraph_msg_deadline_offset",
        "offset of the message from the deadline",
    )
    .unwrap();
    pub static ref MEMGRAPH_CPU_USAGE_GAUGE: IntGauge = register_int_gauge!(
        "memgraph_cpu_usage",
        "CPU usage percentage for the memgraph process"
    )
    .unwrap();
    pub static ref MEMGRAPH_MEM_USAGE_GAUGE: IntGauge = register_int_gauge!(
        "memgraph_memory_usage",
        "Memory usage in bytes for the memgraph process"
    )
    .unwrap();

    // Query-interface memory metrics
    // FalkorDB: derived from `GRAPH.MEMORY USAGE <graph>` (MB).
    pub static ref FALKOR_GRAPH_MEMORY_USAGE_MB: IntGauge = register_int_gauge!(
        "falkordb_graph_memory_usage_mb",
        "Graph memory usage in MB reported by GRAPH.MEMORY USAGE"
    )
    .unwrap();

    // Memgraph: derived from `SHOW STORAGE INFO`.
    pub static ref MEMGRAPH_STORAGE_MEMORY_RES_BYTES: IntGauge = register_int_gauge!(
        "memgraph_storage_memory_res_bytes",
        "Resident memory (bytes) reported by Memgraph SHOW STORAGE INFO"
    )
    .unwrap();
    pub static ref MEMGRAPH_STORAGE_PEAK_MEMORY_RES_BYTES: IntGauge = register_int_gauge!(
        "memgraph_storage_peak_memory_res_bytes",
        "Peak resident memory (bytes) reported by Memgraph SHOW STORAGE INFO"
    )
    .unwrap();
    pub static ref MEMGRAPH_STORAGE_MEMORY_TRACKED_BYTES: IntGauge = register_int_gauge!(
        "memgraph_storage_memory_tracked_bytes",
        "Tracked memory (bytes) reported by Memgraph SHOW STORAGE INFO"
    )
    .unwrap();

    // Memgraph estimate for base dataset storage RAM (bytes).
    // Formula (per Memgraph): StorageRAMUsage = NumberOfVertices×212B + NumberOfEdges×162B
    pub static ref MEMGRAPH_STORAGE_BASE_DATASET_BYTES: IntGauge = register_int_gauge!(
        "memgraph_storage_base_dataset_bytes",
        "Estimated base dataset storage RAM usage in bytes (vertices*212 + edges*162)"
    )
    .unwrap();

    // Precise latency percentiles (microseconds) computed in-process (HDR histogram),
    // exported so the aggregator doesn't need to approximate using Prometheus buckets.
    pub static ref FALKOR_LATENCY_P50_US: IntGauge = register_int_gauge!(
        "falkordb_latency_p50_us",
        "P50 latency in microseconds (computed in-process)"
    )
    .unwrap();
    pub static ref FALKOR_LATENCY_P95_US: IntGauge = register_int_gauge!(
        "falkordb_latency_p95_us",
        "P95 latency in microseconds (computed in-process)"
    )
    .unwrap();
    pub static ref FALKOR_LATENCY_P99_US: IntGauge = register_int_gauge!(
        "falkordb_latency_p99_us",
        "P99 latency in microseconds (computed in-process)"
    )
    .unwrap();

    pub static ref NEO4J_LATENCY_P50_US: IntGauge = register_int_gauge!(
        "neo4j_latency_p50_us",
        "P50 latency in microseconds (computed in-process)"
    )
    .unwrap();
    pub static ref NEO4J_LATENCY_P95_US: IntGauge = register_int_gauge!(
        "neo4j_latency_p95_us",
        "P95 latency in microseconds (computed in-process)"
    )
    .unwrap();
    pub static ref NEO4J_LATENCY_P99_US: IntGauge = register_int_gauge!(
        "neo4j_latency_p99_us",
        "P99 latency in microseconds (computed in-process)"
    )
    .unwrap();

    pub static ref MEMGRAPH_LATENCY_P50_US: IntGauge = register_int_gauge!(
        "memgraph_latency_p50_us",
        "P50 latency in microseconds (computed in-process)"
    )
    .unwrap();
    pub static ref MEMGRAPH_LATENCY_P95_US: IntGauge = register_int_gauge!(
        "memgraph_latency_p95_us",
        "P95 latency in microseconds (computed in-process)"
    )
    .unwrap();
    pub static ref MEMGRAPH_LATENCY_P99_US: IntGauge = register_int_gauge!(
        "memgraph_latency_p99_us",
        "P99 latency in microseconds (computed in-process)"
    )
    .unwrap();

    // Per-query latency percentiles (microseconds), used to build the "single"-style histogram
    // (P10..P99) but for concurrent benchmark runs.
    pub static ref FALKOR_QUERY_LATENCY_PCT_US: IntGaugeVec = register_int_gauge_vec!(
        "falkordb_query_latency_pct_us",
        "Latency percentile per query in microseconds (computed in-process)",
        &["query", "pct"]
    )
    .unwrap();

    pub static ref NEO4J_QUERY_LATENCY_PCT_US: IntGaugeVec = register_int_gauge_vec!(
        "neo4j_query_latency_pct_us",
        "Latency percentile per query in microseconds (computed in-process)",
        &["query", "pct"]
    )
    .unwrap();

    pub static ref MEMGRAPH_QUERY_LATENCY_PCT_US: IntGaugeVec = register_int_gauge_vec!(
        "memgraph_query_latency_pct_us",
        "Latency percentile per query in microseconds (computed in-process)",
        &["query", "pct"]
    )
    .unwrap();
}
