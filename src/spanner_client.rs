use crate::error::BenchmarkError::{OtherError, SpannerError};
use crate::error::BenchmarkResult;
use crate::pokec_cypher_parser::NodeRecord;
use crate::query::QueryParam;
use crate::scheduler::Msg;
use crate::spanner_query::PreparedSpannerQuery;
use crate::{OPERATION_COUNTER, SPANNER_MSG_DEADLINE_OFFSET_GAUGE, SPANNER_STORE_SIZE_BYTES};
use google_cloud_spanner::client::{Client, ClientConfig};
use google_cloud_spanner::mutation::insert_or_update;
use google_cloud_spanner::statement::Statement;
use std::time::Duration;
use tracing::{info, warn};

fn spanner_query_timeout_from_env() -> Duration {
    const DEFAULT_TIMEOUT_MS: u64 = 900_000;

    match std::env::var("SPANNER_QUERY_TIMEOUT_MS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                warn!(
                    "Invalid SPANNER_QUERY_TIMEOUT_MS='{}', using default {}ms",
                    raw, DEFAULT_TIMEOUT_MS
                );
                Duration::from_millis(DEFAULT_TIMEOUT_MS)
            }
        },
        Err(_) => Duration::from_millis(DEFAULT_TIMEOUT_MS),
    }
}

/// Algorithm (graph-procedure) capabilities. Spanner Graph has no Cypher algorithm procedures
/// in this integration; all flags are always `false`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpannerAlgorithmCapabilities {
    pub has_pagerank: bool,
    pub has_max_flow: bool,
    pub has_msf: bool,
    pub has_harmonic: bool,
}

/// Fixture-dependent (vector / fulltext index) capabilities. Out of scope for the baseline
/// Spanner Graph integration.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpannerFixtureCapabilities {
    pub has_vector_query_nodes: bool,
    pub has_fulltext_query_nodes: bool,
    pub has_fulltext_query_relationships: bool,
}

/// Thin wrapper around `google_cloud_spanner::Client` plus the property-graph name used by GQL.
///
/// The underlying Spanner client manages a session pool and is cheap to clone, so concurrent
/// benchmark workers each hold a clone (similar to Mongo).
#[derive(Clone)]
pub struct SpannerClient {
    client: Client,
    database: String,
    graph_name: String,
    query_timeout: Duration,
}

impl SpannerClient {
    pub async fn connect(
        database: &str,
        graph_name: &str,
    ) -> BenchmarkResult<Self> {
        let query_timeout = spanner_query_timeout_from_env();

        let config = if std::env::var("SPANNER_EMULATOR_HOST").is_ok() {
            ClientConfig::default()
        } else {
            ClientConfig::default()
                .with_auth()
                .await
                .map_err(|e| OtherError(format!("Spanner auth failed: {}", e)))?
        };

        let client = Client::new(database, config)
            .await
            .map_err(|e| OtherError(format!("Spanner client connect failed: {}", e)))?;

        info!(
            "Spanner per-query timeout configured to {}ms (database={}, graph={})",
            query_timeout.as_millis(),
            database,
            graph_name
        );

        Ok(SpannerClient {
            client,
            database: database.to_string(),
            graph_name: graph_name.to_string(),
            query_timeout,
        })
    }

    pub fn graph_name(&self) -> &str {
        &self.graph_name
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    /// Apply DDL statements via the Database Admin API and wait for the long-running operation.
    pub async fn update_ddl(
        &self,
        statements: &[String],
    ) -> BenchmarkResult<()> {
        if statements.is_empty() {
            return Ok(());
        }

        use google_cloud_googleapis::spanner::admin::database::v1::UpdateDatabaseDdlRequest;
        use google_cloud_spanner::admin::client::Client as AdminClient;
        use google_cloud_spanner::admin::AdminClientConfig;

        let admin_config = if std::env::var("SPANNER_EMULATOR_HOST").is_ok() {
            AdminClientConfig::default()
        } else {
            AdminClientConfig::default()
                .with_auth()
                .await
                .map_err(|e| OtherError(format!("Spanner admin auth failed: {}", e)))?
        };

        let admin = AdminClient::new(admin_config)
            .await
            .map_err(|e| OtherError(format!("Spanner admin client failed: {}", e)))?;

        let request = UpdateDatabaseDdlRequest {
            database: self.database.clone(),
            statements: statements.to_vec(),
            operation_id: String::new(),
            proto_descriptors: vec![],
        };

        let database_admin = admin.database();
        let mut op = database_admin
            .update_database_ddl(request, None)
            .await
            .map_err(|e| OtherError(format!("Spanner UpdateDatabaseDdl failed: {}", e)))?;

        // Operation::wait -> Result<Option<T>, Status>
        op.wait(None)
            .await
            .map_err(|e| OtherError(format!("Spanner DDL wait failed: {}", e)))?;

        info!("Spanner DDL applied ({} statement(s))", statements.len());
        Ok(())
    }

    pub async fn detect_engine_version(&self) -> BenchmarkResult<Option<String>> {
        if std::env::var("SPANNER_EMULATOR_HOST").is_ok() {
            Ok(Some("Google Spanner Graph (emulator)".to_string()))
        } else {
            Ok(Some("Google Spanner Graph".to_string()))
        }
    }

    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        let users = self.count_table("Users").await?;
        let edges = self.count_table("FriendEdges").await?;
        Ok((users, edges))
    }

    async fn count_table(
        &self,
        table: &str,
    ) -> BenchmarkResult<u64> {
        let sql = format!("SELECT COUNT(*) AS cnt FROM {}", table);
        let stmt = Statement::new(sql);
        let mut tx = self
            .client
            .single()
            .await
            .map_err(|e| OtherError(format!("Spanner single() failed: {}", e)))?;
        let mut iter = tx
            .query(stmt)
            .await
            .map_err(|e| OtherError(format!("Spanner COUNT query failed: {}", e)))?;
        if let Some(row) = iter
            .next()
            .await
            .map_err(|e| OtherError(format!("Spanner COUNT row failed: {}", e)))?
        {
            let cnt: i64 = row
                .column_by_name("cnt")
                .map_err(|e| OtherError(format!("Spanner COUNT decode failed: {}", e)))?;
            Ok(cnt.max(0) as u64)
        } else {
            Ok(0)
        }
    }

    pub async fn store_size_bytes(&self) -> BenchmarkResult<u64> {
        Ok(0)
    }

    pub async fn collect_store_size_metrics(&self) {
        SPANNER_STORE_SIZE_BYTES.set(0);
        match self.store_size_bytes().await {
            Ok(bytes) => SPANNER_STORE_SIZE_BYTES.set(bytes.min(i64::MAX as u64) as i64),
            Err(e) => {
                tracing::debug!("Failed collecting Spanner store size: {}", e);
            }
        }
    }

    pub fn algorithm_capabilities(&self) -> SpannerAlgorithmCapabilities {
        SpannerAlgorithmCapabilities::default()
    }

    pub fn fixture_capabilities(&self) -> SpannerFixtureCapabilities {
        SpannerFixtureCapabilities::default()
    }

    pub async fn insert_users_batch(
        &self,
        nodes: &[NodeRecord],
    ) -> BenchmarkResult<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        let mut mutations = Vec::with_capacity(nodes.len());
        for n in nodes {
            let id = n.id as i64;
            let completion = n.completion_percentage as i64;
            let age = n.age as i64;
            let gender = n.gender.clone();
            mutations.push(insert_or_update(
                "Users",
                &["id", "completion_percentage", "gender", "age"],
                &[&id, &completion, &gender, &age],
            ));
        }
        self.client
            .apply(mutations)
            .await
            .map_err(|e| OtherError(format!("Spanner insert Users failed: {}", e)))?;
        Ok(())
    }

    pub async fn insert_friend_edges_batch(
        &self,
        edges: &[(i64, i64, i64)],
    ) -> BenchmarkResult<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let mut mutations = Vec::with_capacity(edges.len());
        for (src, dst, capacity) in edges {
            mutations.push(insert_or_update(
                "FriendEdges",
                &["src_id", "dst_id", "bench_capacity"],
                &[src, dst, capacity],
            ));
        }
        self.client
            .apply(mutations)
            .await
            .map_err(|e| OtherError(format!("Spanner insert FriendEdges failed: {}", e)))?;
        Ok(())
    }

    fn build_statement(prepared: &PreparedSpannerQuery) -> Statement {
        let mut stmt = Statement::new(prepared.statement.text.clone());
        for (name, value) in &prepared.statement.params {
            match value {
                QueryParam::String(s) => stmt.add_param(name, s),
                QueryParam::Integer(i) => {
                    let v = i64::from(*i);
                    stmt.add_param(name, &v);
                }
                QueryParam::Float(f) => {
                    let v = f64::from(*f);
                    stmt.add_param(name, &v);
                }
                QueryParam::Boolean(b) => stmt.add_param(name, b),
            }
        }
        stmt
    }

    async fn run_statement(
        &self,
        prepared: &PreparedSpannerQuery,
    ) -> BenchmarkResult<()> {
        let stmt = Self::build_statement(prepared);
        let is_write = matches!(prepared.q_type, crate::queries_repository::QueryType::Write);

        if is_write {
            self.client
                .read_write_transaction(move |tx| {
                    let stmt = stmt.clone();
                    Box::pin(async move {
                        tx.update(stmt)
                            .await
                            .map_err(google_cloud_spanner::client::Error::from)?;
                        Ok::<(), google_cloud_spanner::client::Error>(())
                    })
                })
                .await
                .map_err(|e| OtherError(format!("Spanner write failed: {}", e)))?;
        } else {
            let mut tx = self
                .client
                .single()
                .await
                .map_err(|e| OtherError(format!("Spanner single() failed: {}", e)))?;
            let mut iter = tx
                .query(stmt)
                .await
                .map_err(|e| OtherError(format!("Spanner query failed: {}", e)))?;
            while let Some(_row) = iter
                .next()
                .await
                .map_err(|e| OtherError(format!("Spanner row iteration failed: {}", e)))?
            {
            }
        }
        Ok(())
    }

    pub async fn execute_prepared_query<S: AsRef<str>>(
        &self,
        worker_id: S,
        msg: &Msg<PreparedSpannerQuery>,
        simulate: &Option<usize>,
    ) -> BenchmarkResult<()> {
        let worker_id = worker_id.as_ref();
        let q_name = msg.payload.q_name.as_str();
        let timeout = self.query_timeout;
        let offset = msg.compute_offset_ms();

        SPANNER_MSG_DEADLINE_OFFSET_GAUGE.set(offset);
        if offset > 0 {
            tokio::time::sleep(Duration::from_millis(offset as u64)).await;
        }

        if let Some(delay) = simulate {
            if *delay > 0 {
                tokio::time::sleep(Duration::from_millis(*delay as u64)).await;
            }
            return Ok(());
        }

        let result = tokio::time::timeout(timeout, self.run_statement(&msg.payload)).await;

        OPERATION_COUNTER
            .with_label_values(&["spanner", worker_id, "", q_name, "", ""])
            .inc();

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                OPERATION_COUNTER
                    .with_label_values(&["spanner", worker_id, "error", q_name, "", ""])
                    .inc();
                Err(e)
            }
            Err(_) => {
                OPERATION_COUNTER
                    .with_label_values(&["spanner", worker_id, "timeout", q_name, "", ""])
                    .inc();
                Err(OtherError(format!(
                    "Timeout after {}ms",
                    timeout.as_millis()
                )))
            }
        }
    }
}

#[allow(dead_code)]
fn _spanner_error_placeholder(e: google_cloud_spanner::client::Error) -> crate::error::BenchmarkError {
    SpannerError(e.to_string())
}
