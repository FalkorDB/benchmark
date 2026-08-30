use crate::query::QueryParam;
use crate::queries_repository::QueryType;
use serde::{Deserialize, Serialize};

/// A parameterized SQL statement. Params are positional and correspond to `$1`, `$2`, ...
/// placeholders in `text`, mirroring how `query::Query` holds Cypher text + named params.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SqlQuery {
    pub text: String,
    pub params: Vec<QueryParam>,
}

#[derive(Debug, Default, Clone)]
pub struct SqlQueryBuilder {
    query: SqlQuery,
}

impl SqlQueryBuilder {
    pub fn new() -> Self {
        SqlQueryBuilder::default()
    }

    pub fn text(
        mut self,
        text: impl Into<String>,
    ) -> Self {
        self.query.text = text.into();
        self
    }

    pub fn param<V: Into<QueryParam>>(
        mut self,
        value: V,
    ) -> Self {
        self.query.params.push(value.into());
        self
    }

    pub fn build(self) -> SqlQuery {
        self.query
    }
}

/// A prepared SQL query with a stable catalog id, mirroring `queries_repository::PreparedQuery`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedSqlQuery {
    #[serde(default)]
    pub q_id: u16,
    pub q_name: String,
    pub q_type: QueryType,
    pub sql: SqlQuery,
}

impl PreparedSqlQuery {
    pub fn new(
        q_id: u16,
        q_name: String,
        q_type: QueryType,
        sql: SqlQuery,
    ) -> Self {
        Self {
            q_id,
            q_name,
            q_type,
            sql,
        }
    }
}
