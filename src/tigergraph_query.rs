use crate::queries_repository::QueryType;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A TigerGraph operation. Unlike Cypher/SQL/aggregation-pipeline text sent per-request,
/// TigerGraph queries are pre-installed, compiled GSQL procedures (see
/// `src/tigergraph_gsql/*.gsql`); at runtime we only need the installed query's name plus its
/// named parameters, invoked via REST++ `GET/POST /query/{graph}/{name}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TigerGraphOperation {
    RunInstalledQuery {
        name: String,
        params: Map<String, Value>,
    },
}

impl TigerGraphOperation {
    pub fn installed_query_name(&self) -> &str {
        match self {
            TigerGraphOperation::RunInstalledQuery { name, .. } => name,
        }
    }

    pub fn params(&self) -> &Map<String, Value> {
        match self {
            TigerGraphOperation::RunInstalledQuery { params, .. } => params,
        }
    }
}

/// A prepared TigerGraph query with a stable catalog id, mirroring
/// `sql_query::PreparedSqlQuery`/`queries_repository::PreparedQuery`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedTigerGraphQuery {
    #[serde(default)]
    pub q_id: u16,
    pub q_name: String,
    pub q_type: QueryType,
    pub operation: TigerGraphOperation,
}

impl PreparedTigerGraphQuery {
    pub fn new(
        q_id: u16,
        q_name: String,
        q_type: QueryType,
        operation: TigerGraphOperation,
    ) -> Self {
        Self {
            q_id,
            q_name,
            q_type,
            operation,
        }
    }
}

/// Small builder for `TigerGraphOperation::RunInstalledQuery`, mirroring `SqlQueryBuilder`'s
/// ergonomics: pick the installed query name and bind its named GSQL parameters.
#[derive(Debug, Default, Clone)]
pub struct TigerGraphQueryBuilder {
    name: String,
    params: Map<String, Value>,
}

impl TigerGraphQueryBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        TigerGraphQueryBuilder {
            name: name.into(),
            params: Map::new(),
        }
    }

    pub fn param<V: Into<Value>>(
        mut self,
        key: impl Into<String>,
        value: V,
    ) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> TigerGraphOperation {
        TigerGraphOperation::RunInstalledQuery {
            name: self.name,
            params: self.params,
        }
    }
}
