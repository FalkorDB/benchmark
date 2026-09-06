use crate::error::BenchmarkError::{OtherError, PostgresError};
use crate::error::BenchmarkResult;
use crate::query::QueryParam;
use crate::scheduler::Msg;
use crate::sql_query::PreparedSqlQuery;
use crate::{OPERATION_COUNTER, POSTGRES_MSG_DEADLINE_OFFSET_GAUGE, POSTGRES_STORE_SIZE_BYTES};
use std::sync::Arc;
use std::time::Duration;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, NoTls};
use tracing::{error, info, warn};

fn postgres_query_timeout_from_env() -> Duration {
    const DEFAULT_TIMEOUT_MS: u64 = 900_000;

    match std::env::var("POSTGRES_QUERY_TIMEOUT_MS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                warn!(
                    "Invalid POSTGRES_QUERY_TIMEOUT_MS='{}', using default {}ms",
                    raw, DEFAULT_TIMEOUT_MS
                );
                Duration::from_millis(DEFAULT_TIMEOUT_MS)
            }
        },
        Err(_) => Duration::from_millis(DEFAULT_TIMEOUT_MS),
    }
}

/// A single live connection to Postgres plus its configured query timeout.
///
/// PostgreSQL uses a process-per-connection model server-side: each connection is served by its
/// own dedicated backend process, which can only ever occupy one CPU core. `PostgresClient`
/// (below) pools several `PostgresConnection`s so concurrent benchmark workers can each drive
/// their own backend process instead of serializing all queries through a single one.
#[derive(Clone)]
struct PostgresConnection {
    client: Arc<Client>,
    query_timeout: Duration,
}

/// Algorithm (graph-procedure) capabilities. Postgres has no equivalent to the Cypher algorithm
/// procedures (pageRank, maxFlow, MSF, harmonic centrality), so all flags are always `false`.
/// This exists purely so call sites that check vendor capabilities compile uniformly.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostgresAlgorithmCapabilities {
    pub has_pagerank: bool,
    pub has_max_flow: bool,
    pub has_msf: bool,
    pub has_harmonic: bool,
}

/// Fixture-dependent (vector / fulltext index) capabilities. Postgres support for these query
/// families is out of scope; all flags are always `false`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostgresFixtureCapabilities {
    pub has_vector_query_nodes: bool,
    pub has_fulltext_query_nodes: bool,
    pub has_fulltext_query_relationships: bool,
}

fn to_sql_params(params: &[QueryParam]) -> Vec<Box<dyn ToSql + Sync + Send>> {
    params
        .iter()
        .map(|p| -> Box<dyn ToSql + Sync + Send> {
            match p {
                QueryParam::String(s) => Box::new(s.clone()),
                QueryParam::Integer(i) => Box::new(*i),
                QueryParam::Float(f) => Box::new(*f),
                QueryParam::Boolean(b) => Box::new(*b),
            }
        })
        .collect()
}

impl PostgresConnection {
    async fn connect(config: &str) -> BenchmarkResult<Self> {
        let (client, connection) = tokio_postgres::connect(config, NoTls)
            .await
            .map_err(PostgresError)?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("Postgres connection error: {}", e);
            }
        });

        Ok(PostgresConnection {
            client: Arc::new(client),
            query_timeout: postgres_query_timeout_from_env(),
        })
    }

    async fn execute_ddl(
        &self,
        sql: &str,
    ) -> BenchmarkResult<()> {
        self.client.batch_execute(sql).await.map_err(PostgresError)?;
        Ok(())
    }

    async fn detect_engine_version(&self) -> BenchmarkResult<Option<String>> {
        let row = self
            .client
            .query_opt("SHOW server_version", &[])
            .await
            .map_err(PostgresError)?;
        Ok(row
            .and_then(|r| r.try_get::<_, String>(0).ok())
            .map(|v| format!("PostgreSQL {}", v)))
    }

    async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        let users_row = self
            .client
            .query_one("SELECT count(*) AS cnt FROM users", &[])
            .await
            .map_err(PostgresError)?;
        let users_count: i64 = users_row.get("cnt");

        let edges_row = self
            .client
            .query_one("SELECT count(*) AS cnt FROM friend_edges", &[])
            .await
            .map_err(PostgresError)?;
        let edges_count: i64 = edges_row.get("cnt");

        Ok((users_count.max(0) as u64, edges_count.max(0) as u64))
    }

    async fn store_size_bytes(&self) -> BenchmarkResult<u64> {
        let row = self
            .client
            .query_one(
                "SELECT pg_total_relation_size('users') + pg_total_relation_size('friend_edges') AS bytes",
                &[],
            )
            .await
            .map_err(PostgresError)?;
        let bytes: i64 = row.get("bytes");
        Ok(bytes.max(0) as u64)
    }

    async fn execute_prepared_query<S: AsRef<str>>(
        &self,
        worker_id: S,
        msg: &Msg<PreparedSqlQuery>,
        simulate: &Option<usize>,
    ) -> BenchmarkResult<()> {
        let worker_id = worker_id.as_ref();
        let q_name = msg.payload.q_name.as_str();
        let timeout = self.query_timeout;
        let offset = msg.compute_offset_ms();

        POSTGRES_MSG_DEADLINE_OFFSET_GAUGE.set(offset);
        if offset > 0 {
            tokio::time::sleep(Duration::from_millis(offset as u64)).await;
        }

        if let Some(delay) = simulate {
            if *delay > 0 {
                tokio::time::sleep(Duration::from_millis(*delay as u64)).await;
            }
            return Ok(());
        }

        let sql_text = msg.payload.sql.text.as_str();
        let boxed_params = to_sql_params(&msg.payload.sql.params);
        let param_refs: Vec<&(dyn ToSql + Sync)> = boxed_params
            .iter()
            .map(|b| b.as_ref() as &(dyn ToSql + Sync))
            .collect();

        let result = tokio::time::timeout(timeout, self.client.query(sql_text, &param_refs)).await;

        OPERATION_COUNTER
            .with_label_values(&["postgres", worker_id, "", q_name, "", ""])
            .inc();

        match result {
            Ok(Ok(_rows)) => Ok(()),
            Ok(Err(e)) => {
                OPERATION_COUNTER
                    .with_label_values(&["postgres", worker_id, "error", q_name, "", ""])
                    .inc();
                Err(PostgresError(e))
            }
            Err(_) => {
                OPERATION_COUNTER
                    .with_label_values(&["postgres", worker_id, "timeout", q_name, "", ""])
                    .inc();
                Err(OtherError(format!(
                    "Timeout after {}ms",
                    timeout.as_millis()
                )))
            }
        }
    }
}

/// A pool of independent Postgres connections.
///
/// PostgreSQL's server-side process-per-connection model means a single shared connection can
/// only ever occupy one CPU core, no matter how many benchmark workers share it (queries just
/// pipeline through the one backend process). To enable true parallel execution across cores,
/// `connect` opens `pool_size` independent connections up front, and each benchmark worker is
/// handed one dedicated connection (round-robin by worker index, see `worker_connection`) for the
/// duration of the run. With a pool of size N, up to N workers can have queries executing
/// concurrently on N separate Postgres backend processes/cores.
#[derive(Clone)]
pub struct PostgresClient {
    connections: Vec<PostgresConnection>,
}

/// A handle to a single connection drawn from a `PostgresClient` pool, assigned to one benchmark
/// worker for the duration of a run.
#[derive(Clone)]
pub struct PostgresWorkerClient(PostgresConnection);

impl PostgresWorkerClient {
    pub async fn execute_prepared_query<S: AsRef<str>>(
        &self,
        worker_id: S,
        msg: &Msg<PreparedSqlQuery>,
        simulate: &Option<usize>,
    ) -> BenchmarkResult<()> {
        self.0.execute_prepared_query(worker_id, msg, simulate).await
    }
}

impl PostgresClient {
    /// Opens a pool of `pool_size` independent connections to Postgres (clamped to at least 1).
    /// Use a `pool_size` matching the benchmark's `--parallel` worker count to let every worker
    /// run against its own backend process.
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        dbname: &str,
        pool_size: usize,
    ) -> BenchmarkResult<Self> {
        let pool_size = pool_size.max(1);
        let config = format!(
            "host={} port={} user={} password={} dbname={} connect_timeout=10",
            host, port, user, password, dbname
        );

        let mut connections = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            connections.push(PostgresConnection::connect(&config).await?);
        }

        info!(
            "Postgres per-query timeout configured to {}ms",
            connections[0].query_timeout.as_millis()
        );
        info!(
            "Postgres connection pool established with {} connection(s); up to {} workers can run concurrently on separate backend processes/cores",
            pool_size, pool_size
        );

        Ok(PostgresClient { connections })
    }

    fn primary(&self) -> &PostgresConnection {
        &self.connections[0]
    }

    pub async fn execute_ddl(
        &self,
        sql: &str,
    ) -> BenchmarkResult<()> {
        self.primary().execute_ddl(sql).await
    }

    pub async fn detect_engine_version(&self) -> BenchmarkResult<Option<String>> {
        self.primary().detect_engine_version().await
    }

    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        self.primary().graph_size().await
    }

    pub async fn store_size_bytes(&self) -> BenchmarkResult<u64> {
        self.primary().store_size_bytes().await
    }

    /// Best-effort: query Postgres for combined table+index size and write it into the
    /// corresponding Prometheus gauge.
    pub async fn collect_store_size_metrics(&self) {
        POSTGRES_STORE_SIZE_BYTES.set(0);
        match self.store_size_bytes().await {
            Ok(bytes) => POSTGRES_STORE_SIZE_BYTES.set(bytes.min(i64::MAX as u64) as i64),
            Err(e) => {
                tracing::debug!("Failed collecting Postgres store size: {}", e);
            }
        }
    }

    /// Postgres has no algorithm procedures; these are always unsupported.
    pub fn algorithm_capabilities(&self) -> PostgresAlgorithmCapabilities {
        PostgresAlgorithmCapabilities::default()
    }

    /// Postgres has no vector/fulltext index procedures in this integration; always unsupported.
    pub fn fixture_capabilities(&self) -> PostgresFixtureCapabilities {
        PostgresFixtureCapabilities::default()
    }

    /// Returns the connection assigned to `worker_index`, distributing workers round-robin
    /// across the pool so each worker's queries run against its own dedicated Postgres backend
    /// process (and thus its own CPU core) rather than contending on a single shared connection.
    pub fn worker_connection(
        &self,
        worker_index: usize,
    ) -> PostgresWorkerClient {
        PostgresWorkerClient(self.connections[worker_index % self.connections.len()].clone())
    }
}
