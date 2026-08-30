use crate::error::BenchmarkError::{MongoError, OtherError};
use crate::error::BenchmarkResult;
use crate::mongo_query::{MongoOperation, PreparedMongoQuery};
use crate::scheduler::Msg;
use crate::{MONGO_MSG_DEADLINE_OFFSET_GAUGE, MONGO_STORE_SIZE_BYTES, OPERATION_COUNTER};
use futures::TryStreamExt;
use mongodb::bson::{doc, Document};
use mongodb::options::UpdateOptions;
use mongodb::{Client, Database};
use std::time::Duration;
use tracing::{info, warn};

fn mongo_query_timeout_from_env() -> Duration {
    const DEFAULT_TIMEOUT_MS: u64 = 900_000;

    match std::env::var("MONGO_QUERY_TIMEOUT_MS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                warn!(
                    "Invalid MONGO_QUERY_TIMEOUT_MS='{}', using default {}ms",
                    raw, DEFAULT_TIMEOUT_MS
                );
                Duration::from_millis(DEFAULT_TIMEOUT_MS)
            }
        },
        Err(_) => Duration::from_millis(DEFAULT_TIMEOUT_MS),
    }
}

/// Thin wrapper around a `mongodb::Database` handle. `mongodb::Client` (and the `Database`
/// handles derived from it) already manage an internal connection pool and are cheap to clone,
/// mirroring how `PostgresClient` shares a single connection across benchmark workers.
#[derive(Clone)]
pub struct MongoClient {
    db: Database,
    query_timeout: Duration,
}

/// Algorithm (graph-procedure) capabilities. MongoDB has no equivalent to the Cypher algorithm
/// procedures (pageRank, maxFlow, MSF, harmonic centrality), so all flags are always `false`.
/// This exists purely so call sites that check vendor capabilities compile uniformly.
#[derive(Debug, Clone, Copy, Default)]
pub struct MongoAlgorithmCapabilities {
    pub has_pagerank: bool,
    pub has_max_flow: bool,
    pub has_msf: bool,
    pub has_harmonic: bool,
}

/// Fixture-dependent (vector / fulltext index) capabilities. Mongo support for these query
/// families is out of scope; all flags are always `false`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MongoFixtureCapabilities {
    pub has_vector_query_nodes: bool,
    pub has_fulltext_query_nodes: bool,
    pub has_fulltext_query_relationships: bool,
}

impl MongoClient {
    pub async fn connect(
        uri: &str,
        dbname: &str,
    ) -> BenchmarkResult<Self> {
        let client = Client::with_uri_str(uri).await.map_err(MongoError)?;
        let db = client.database(dbname);

        let query_timeout = mongo_query_timeout_from_env();
        info!(
            "Mongo per-query timeout configured to {}ms",
            query_timeout.as_millis()
        );

        Ok(MongoClient { db, query_timeout })
    }

    fn collection(
        &self,
        name: &str,
    ) -> mongodb::Collection<Document> {
        self.db.collection::<Document>(name)
    }

    /// Exposed for the bulk `insertMany`-based dataset loader (`mongo::load_from_cypher_import`),
    /// which needs direct collection handles rather than going through `execute_prepared_query`.
    pub fn users_collection(&self) -> mongodb::Collection<Document> {
        self.collection("users")
    }

    pub fn friend_edges_collection(&self) -> mongodb::Collection<Document> {
        self.collection("friend_edges")
    }

    pub async fn drop_collections(&self) -> BenchmarkResult<()> {
        self.collection("friend_edges").drop(None).await.map_err(MongoError)?;
        self.collection("users").drop(None).await.map_err(MongoError)?;
        Ok(())
    }

    /// Create indexes on `friend_edges` (src/dst lookups + a unique compound index to dedupe
    /// edges) and `users` (age, used by several filtered-expansion query families).
    pub async fn create_schema(&self) -> BenchmarkResult<()> {
        use mongodb::options::IndexOptions;
        use mongodb::IndexModel;

        let friend_edges = self.collection("friend_edges");
        friend_edges
            .create_index(IndexModel::builder().keys(doc! {"src": 1}).build(), None)
            .await
            .map_err(MongoError)?;
        friend_edges
            .create_index(IndexModel::builder().keys(doc! {"dst": 1}).build(), None)
            .await
            .map_err(MongoError)?;
        friend_edges
            .create_index(
                IndexModel::builder()
                    .keys(doc! {"src": 1, "dst": 1})
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
                None,
            )
            .await
            .map_err(MongoError)?;

        let users = self.collection("users");
        users
            .create_index(IndexModel::builder().keys(doc! {"age": 1}).build(), None)
            .await
            .map_err(MongoError)?;

        Ok(())
    }

    pub async fn detect_engine_version(&self) -> BenchmarkResult<Option<String>> {
        let result = self
            .db
            .run_command(doc! {"buildInfo": 1}, None)
            .await
            .map_err(MongoError)?;
        Ok(result
            .get_str("version")
            .ok()
            .map(|v| format!("MongoDB {}", v)))
    }

    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        let users_count = self
            .collection("users")
            .estimated_document_count(None)
            .await
            .map_err(MongoError)?;
        let edges_count = self
            .collection("friend_edges")
            .estimated_document_count(None)
            .await
            .map_err(MongoError)?;
        Ok((users_count, edges_count))
    }

    pub async fn store_size_bytes(&self) -> BenchmarkResult<u64> {
        let stats = self
            .db
            .run_command(doc! {"dbStats": 1}, None)
            .await
            .map_err(MongoError)?;
        let bytes = stats.get_f64("storageSize").unwrap_or(0.0)
            + stats.get_f64("indexSize").unwrap_or(0.0);
        Ok(bytes.max(0.0) as u64)
    }

    /// Best-effort: query MongoDB for combined collection+index size and write it into the
    /// corresponding Prometheus gauge.
    pub async fn collect_store_size_metrics(&self) {
        MONGO_STORE_SIZE_BYTES.set(0);
        match self.store_size_bytes().await {
            Ok(bytes) => MONGO_STORE_SIZE_BYTES.set(bytes.min(i64::MAX as u64) as i64),
            Err(e) => {
                tracing::debug!("Failed collecting Mongo store size: {}", e);
            }
        }
    }

    /// MongoDB has no algorithm procedures; these are always unsupported.
    pub fn algorithm_capabilities(&self) -> MongoAlgorithmCapabilities {
        MongoAlgorithmCapabilities::default()
    }

    /// MongoDB has no vector/fulltext index procedures in this integration; always unsupported.
    pub fn fixture_capabilities(&self) -> MongoFixtureCapabilities {
        MongoFixtureCapabilities::default()
    }

    async fn run_operation(
        &self,
        operation: &MongoOperation,
    ) -> BenchmarkResult<()> {
        match operation {
            MongoOperation::Find { collection, filter } => {
                self.collection(collection)
                    .find_one(filter.clone(), None)
                    .await
                    .map_err(MongoError)?;
            }
            MongoOperation::Aggregate { collection, pipeline } => {
                let mut cursor = self
                    .collection(collection)
                    .aggregate(pipeline.clone(), None)
                    .await
                    .map_err(MongoError)?;
                // Drain the cursor so the aggregation actually executes end-to-end.
                while cursor.try_next().await.map_err(MongoError)?.is_some() {}
            }
            MongoOperation::InsertOne { collection, document } => {
                // Ignore duplicate-key errors so re-running the same generated workload against
                // an already-populated collection behaves like Postgres's `ON CONFLICT DO NOTHING`.
                if let Err(e) = self.collection(collection).insert_one(document.clone(), None).await {
                    if !is_duplicate_key_error(&e) {
                        return Err(MongoError(e));
                    }
                }
            }
            MongoOperation::UpdateOne {
                collection,
                filter,
                update,
                upsert,
            } => {
                let options = UpdateOptions::builder().upsert(*upsert).build();
                self.collection(collection)
                    .update_one(filter.clone(), update.clone(), options)
                    .await
                    .map_err(MongoError)?;
            }
            MongoOperation::DeleteOne { collection, filter } => {
                self.collection(collection)
                    .delete_one(filter.clone(), None)
                    .await
                    .map_err(MongoError)?;
            }
        }
        Ok(())
    }

    pub async fn execute_prepared_query<S: AsRef<str>>(
        &self,
        worker_id: S,
        msg: &Msg<PreparedMongoQuery>,
        simulate: &Option<usize>,
    ) -> BenchmarkResult<()> {
        let worker_id = worker_id.as_ref();
        let q_name = msg.payload.q_name.as_str();
        let timeout = self.query_timeout;
        let offset = msg.compute_offset_ms();

        MONGO_MSG_DEADLINE_OFFSET_GAUGE.set(offset);
        if offset > 0 {
            tokio::time::sleep(Duration::from_millis(offset as u64)).await;
        }

        if let Some(delay) = simulate {
            if *delay > 0 {
                tokio::time::sleep(Duration::from_millis(*delay as u64)).await;
            }
            return Ok(());
        }

        let result = tokio::time::timeout(timeout, self.run_operation(&msg.payload.operation)).await;

        OPERATION_COUNTER
            .with_label_values(&["mongo", worker_id, "", q_name, "", ""])
            .inc();

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                OPERATION_COUNTER
                    .with_label_values(&["mongo", worker_id, "error", q_name, "", ""])
                    .inc();
                Err(e)
            }
            Err(_) => {
                OPERATION_COUNTER
                    .with_label_values(&["mongo", worker_id, "timeout", q_name, "", ""])
                    .inc();
                Err(OtherError(format!(
                    "Timeout after {}ms",
                    timeout.as_millis()
                )))
            }
        }
    }
}

fn is_duplicate_key_error(err: &mongodb::error::Error) -> bool {
    use mongodb::error::ErrorKind;
    matches!(err.kind.as_ref(), ErrorKind::Write(_)) && err.to_string().contains("E11000")
}
