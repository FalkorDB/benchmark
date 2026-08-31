use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::scheduler::Msg;
use crate::tigergraph_query::{PreparedTigerGraphQuery, TigerGraphOperation};
use crate::{OPERATION_COUNTER, TIGERGRAPH_MSG_DEADLINE_OFFSET_GAUGE, TIGERGRAPH_STORE_SIZE_BYTES};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Name of the single named graph every installed GSQL query in `src/tigergraph_gsql/` targets
/// via `FOR GRAPH benchmark_graph`.
pub const TIGERGRAPH_GRAPH_NAME: &str = "benchmark_graph";

fn tigergraph_query_timeout_from_env() -> Duration {
    const DEFAULT_TIMEOUT_MS: u64 = 900_000;

    match std::env::var("TIGERGRAPH_QUERY_TIMEOUT_MS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                warn!(
                    "Invalid TIGERGRAPH_QUERY_TIMEOUT_MS='{}', using default {}ms",
                    raw, DEFAULT_TIMEOUT_MS
                );
                Duration::from_millis(DEFAULT_TIMEOUT_MS)
            }
        },
        Err(_) => Duration::from_millis(DEFAULT_TIMEOUT_MS),
    }
}

/// Algorithm (graph-procedure) capabilities. Unlike Postgres/Mongo, TigerGraph is a native graph
/// engine and installs GSQL implementations of every algorithm family, so all flags are `true`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TigerGraphAlgorithmCapabilities {
    pub has_pagerank: bool,
    pub has_max_flow: bool,
    pub has_msf: bool,
    pub has_harmonic: bool,
}

impl TigerGraphAlgorithmCapabilities {
    fn all_supported() -> Self {
        TigerGraphAlgorithmCapabilities {
            has_pagerank: true,
            has_max_flow: true,
            has_msf: true,
            has_harmonic: true,
        }
    }
}

/// Fixture-dependent (vector / fulltext index) capabilities. Out of scope this phase (would
/// require TigerGraph's newer TigerVector feature / a separate text-search integration); all
/// flags are always `false`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TigerGraphFixtureCapabilities {
    pub has_vector_query_nodes: bool,
    pub has_fulltext_query_nodes: bool,
    pub has_fulltext_query_relationships: bool,
}

/// Thin `reqwest`-based wrapper around TigerGraph's REST++ API (default port 9000, used for
/// installed-query execution and built-in stats) and the GSQL server's script-execution endpoint
/// (default port 14240, used once during schema/query install). There is no official Rust client
/// for TigerGraph, so — consistent with this codebase's per-vendor `*_client.rs` pattern — this is
/// a small hand-rolled wrapper rather than a heavyweight dependency.
#[derive(Clone)]
pub struct TigerGraphClient {
    http: reqwest::Client,
    rest_base_url: String,
    gsql_base_url: String,
    graph_name: String,
    username: String,
    password: String,
    bearer_token: Arc<RwLock<Option<String>>>,
    query_timeout: Duration,
}

impl TigerGraphClient {
    pub async fn connect(
        rest_base_url: &str,
        gsql_base_url: &str,
        username: &str,
        password: &str,
        graph_name: &str,
    ) -> BenchmarkResult<Self> {
        let http = reqwest::Client::builder().build()?;
        let client = TigerGraphClient {
            http,
            rest_base_url: rest_base_url.trim_end_matches('/').to_string(),
            gsql_base_url: gsql_base_url.trim_end_matches('/').to_string(),
            graph_name: graph_name.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            bearer_token: Arc::new(RwLock::new(None)),
            query_timeout: tigergraph_query_timeout_from_env(),
        };
        client.refresh_token().await;
        Ok(client)
    }

    /// Best-effort bearer-token acquisition via `POST /requesttoken`. TigerGraph Community
    /// Edition frequently runs with authentication disabled entirely, so a failure here is not
    /// fatal: subsequent requests simply proceed without an `Authorization` header.
    async fn refresh_token(&self) {
        let url = format!("{}/requesttoken", self.rest_base_url);
        let body = serde_json::json!({ "graph": self.graph_name });

        match self.http.post(&url).json(&body).send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<Value>().await {
                    Ok(json) => {
                        if let Some(token) = json.get("token").and_then(|t| t.as_str()) {
                            *self.bearer_token.write().await = Some(token.to_string());
                            info!("Acquired TigerGraph REST++ bearer token");
                            return;
                        }
                        debug!("TigerGraph /requesttoken response did not include a token");
                    }
                    Err(e) => debug!("Failed to parse TigerGraph /requesttoken response: {}", e),
                }
            }
            Ok(response) => {
                debug!(
                    "TigerGraph /requesttoken returned {}; proceeding without bearer auth",
                    response.status()
                );
            }
            Err(e) => {
                debug!(
                    "TigerGraph /requesttoken request failed ({}); proceeding without bearer auth",
                    e
                );
            }
        }
    }

    async fn authorize(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        match self.bearer_token.read().await.clone() {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        }
    }

    /// Execute a raw GSQL script (schema DDL or `CREATE QUERY`/`INSTALL QUERY` statements)
    /// against the GSQL server's script-execution endpoint, authenticated via HTTP Basic auth.
    ///
    /// TigerGraph 4.x removed the legacy `/gsqlserver/gsql/file` multipart file-upload endpoint
    /// in favor of `/gsql/v1/statements`, which accepts a raw `text/plain` body containing one or
    /// more newline-separated GSQL statements (verified against a live 4.2.4 Community Edition
    /// server: multi-statement scripts, including a `CREATE GRAPH` that takes ~30s while GPE/GSE
    /// reload, complete correctly in a single request).
    ///
    /// Unlike the legacy endpoint, `/gsql/v1/statements` always responds with HTTP 200 and reports
    /// failures only via plain-text messages in the body (e.g. `"Semantic Check Fails: ..."`,
    /// `"Failed to create vertex types: ..."`, or a parser `"Encountered ..."` message), so errors
    /// must be detected by scanning the response text rather than the HTTP status code.
    pub async fn execute_gsql_script(
        &self,
        script: &str,
    ) -> BenchmarkResult<()> {
        let url = format!("{}/gsql/v1/statements", self.gsql_base_url);
        let response = self
            .http
            .post(&url)
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "text/plain")
            .body(script.to_string())
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if !status.is_success() || Self::gsql_response_indicates_failure(&text) {
            return Err(OtherError(format!(
                "TigerGraph GSQL script execution failed ({}): {}",
                status, text
            )));
        }
        Ok(())
    }

    /// Heuristically detect failure markers in a `/gsql/v1/statements` plain-text response, since
    /// the endpoint reports errors via message content rather than HTTP status codes.
    fn gsql_response_indicates_failure(text: &str) -> bool {
        const FAILURE_MARKERS: [&str; 6] = [
            "semantic check fail",
            "failed to",
            "encountered \"",
            "error:",
            "gsql error",
            "could not be",
        ];
        let lower = text.to_lowercase();
        FAILURE_MARKERS.iter().any(|marker| lower.contains(marker))
    }

    pub async fn detect_engine_version(&self) -> BenchmarkResult<Option<String>> {
        let url = format!("{}/version", self.rest_base_url);
        let request = self.authorize(self.http.get(&url)).await;
        let response = request.send().await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let json: Value = response.json().await.unwrap_or(Value::Null);
        Ok(Some(
            json.get("message")
                .and_then(|m| m.as_str())
                .map(|v| format!("TigerGraph ({v})"))
                .unwrap_or_else(|| "TigerGraph".to_string()),
        ))
    }

    async fn stat_count(
        &self,
        function: &str,
        type_name: &str,
    ) -> u64 {
        let url = format!("{}/builtins/{}", self.rest_base_url, self.graph_name);
        let body = serde_json::json!({ "function": function, "type": type_name });
        let request = self.authorize(self.http.post(&url).json(&body)).await;

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                debug!("TigerGraph builtins '{}' call failed: {}", function, e);
                return 0;
            }
        };
        if !response.status().is_success() {
            return 0;
        }
        // The REST++ response shape is e.g.
        // `{"results":[{"v_type":"User","count":10000}]}` — the count is always under the
        // `count` key, not under a key named after `type_name` (which only ever appears as the
        // *value* of the `v_type`/`e_type` field).
        let json: Value = response.json().await.unwrap_or(Value::Null);
        json.get("results")
            .and_then(|r| r.as_array())
            .and_then(|arr| arr.first())
            .and_then(|entry| entry.get("count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    }

    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        let vertex_count = self.stat_count("stat_vertex_number", "User").await;
        let edge_count = self.stat_count("stat_edge_number", "Friend").await;
        Ok((vertex_count, edge_count))
    }

    /// Batch-upsert vertices and/or edges via the REST++ built-in `POST /graph/{graph}` endpoint,
    /// which accepts a JSON body shaped like
    /// `{"vertices": {"User": {"<id>": {"attr": {"value": ...}}}}, "edges": {...}}`.
    pub async fn upsert_graph_data(
        &self,
        body: &Value,
    ) -> BenchmarkResult<()> {
        let url = format!("{}/graph/{}", self.rest_base_url, self.graph_name);
        let request = self.authorize(self.http.post(&url).json(body)).await;
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(OtherError(format!(
                "TigerGraph batch upsert failed ({}): {}",
                status, text
            )));
        }
        Ok(())
    }

    /// TigerGraph does not expose a simple, universally available REST endpoint for on-disk
    /// footprint the way Postgres's `pg_total_relation_size`/Mongo's `$collStats` do; this is a
    /// best-effort stub, consistent with how external Postgres/Mongo endpoints fall back to `0MB`
    /// RAM and a dataset-bytes estimate instead.
    pub async fn store_size_bytes(&self) -> BenchmarkResult<u64> {
        Err(OtherError(
            "TigerGraph store-size collection is not implemented for this integration"
                .to_string(),
        ))
    }

    pub async fn collect_store_size_metrics(&self) {
        TIGERGRAPH_STORE_SIZE_BYTES.set(0);
        if let Err(e) = self.store_size_bytes().await {
            debug!("Skipping TigerGraph store size collection: {}", e);
        }
    }

    /// TigerGraph natively installs GSQL implementations of every algorithm family (unlike
    /// Postgres/Mongo, which have no equivalent).
    pub fn algorithm_capabilities(&self) -> TigerGraphAlgorithmCapabilities {
        TigerGraphAlgorithmCapabilities::all_supported()
    }

    /// Vector/fulltext fixture support is out of scope this phase; always unsupported.
    pub fn fixture_capabilities(&self) -> TigerGraphFixtureCapabilities {
        TigerGraphFixtureCapabilities::default()
    }

    fn param_to_query_string(value: &Value) -> String {
        match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        }
    }

    pub async fn execute_prepared_query<S: AsRef<str>>(
        &self,
        worker_id: S,
        msg: &Msg<PreparedTigerGraphQuery>,
        simulate: &Option<usize>,
    ) -> BenchmarkResult<()> {
        let worker_id = worker_id.as_ref();
        let q_name = msg.payload.q_name.as_str();
        let timeout = self.query_timeout;
        let offset = msg.compute_offset_ms();

        TIGERGRAPH_MSG_DEADLINE_OFFSET_GAUGE.set(offset);
        if offset > 0 {
            tokio::time::sleep(Duration::from_millis(offset as u64)).await;
        }

        if let Some(delay) = simulate {
            if *delay > 0 {
                tokio::time::sleep(Duration::from_millis(*delay as u64)).await;
            }
            return Ok(());
        }

        let TigerGraphOperation::RunInstalledQuery { name, params } = &msg.payload.operation;
        let base_url = format!("{}/query/{}/{}", self.rest_base_url, self.graph_name, name);
        let query_pairs: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.clone(), Self::param_to_query_string(v)))
            .collect();
        let url = url::Url::parse_with_params(&base_url, &query_pairs)
            .map_err(|e| OtherError(format!("Failed to build TigerGraph query URL: {}", e)))?;

        let request = self.http.get(url);
        let request = self.authorize(request).await;

        let result = tokio::time::timeout(timeout, request.send()).await;

        OPERATION_COUNTER
            .with_label_values(&["tigergraph", worker_id, "", q_name, "", ""])
            .inc();

        match result {
            Ok(Ok(response)) if response.status().is_success() => Ok(()),
            Ok(Ok(response)) => {
                OPERATION_COUNTER
                    .with_label_values(&["tigergraph", worker_id, "error", q_name, "", ""])
                    .inc();
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                Err(OtherError(format!(
                    "TigerGraph query '{}' failed ({}): {}",
                    name, status, text
                )))
            }
            Ok(Err(e)) => {
                OPERATION_COUNTER
                    .with_label_values(&["tigergraph", worker_id, "error", q_name, "", ""])
                    .inc();
                Err(e.into())
            }
            Err(_) => {
                OPERATION_COUNTER
                    .with_label_values(&["tigergraph", worker_id, "timeout", q_name, "", ""])
                    .inc();
                Err(OtherError(format!(
                    "Timeout after {}ms",
                    timeout.as_millis()
                )))
            }
        }
    }
}
