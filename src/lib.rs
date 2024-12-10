use lazy_static::lazy_static;
use prometheus::register_counter_vec;
use prometheus::register_histogram;
use prometheus::register_int_counter;
use prometheus::register_int_gauge;
use prometheus::CounterVec;
use prometheus::Histogram;
use prometheus::IntCounter;
use prometheus::IntGauge;

pub mod cli;
pub mod error;
pub mod falkor;
pub mod neo4j;
pub mod neo4j_client;
pub mod process_monitor;
pub mod prometheus_endpoint;
pub mod queries_repository;
pub mod query;
pub mod scenario;
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
}
