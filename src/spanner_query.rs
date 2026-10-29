use crate::queries_repository::QueryType;
use crate::query::QueryParam;
use serde::{Deserialize, Serialize};

/// A parameterized Spanner statement. Params are named (`@id`, `@src`, ...) matching Spanner's
/// SQL/GQL bind style — unlike Postgres positional `$1` placeholders.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SpannerStatement {
    pub text: String,
    pub params: Vec<(String, QueryParam)>,
}

#[derive(Debug, Default, Clone)]
pub struct SpannerStatementBuilder {
    statement: SpannerStatement,
}

impl SpannerStatementBuilder {
    pub fn new() -> Self {
        SpannerStatementBuilder::default()
    }

    pub fn text(
        mut self,
        text: impl Into<String>,
    ) -> Self {
        self.statement.text = text.into();
        self
    }

    pub fn param(
        mut self,
        name: impl Into<String>,
        value: impl Into<QueryParam>,
    ) -> Self {
        self.statement.params.push((name.into(), value.into()));
        self
    }

    pub fn build(self) -> SpannerStatement {
        self.statement
    }
}

/// A prepared Spanner query with a stable catalog id, mirroring `sql_query::PreparedSqlQuery`.
/// `statement.text` may be GoogleSQL DML/DQL or Spanner Graph GQL (must start with `GRAPH`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedSpannerQuery {
    #[serde(default)]
    pub q_id: u16,
    pub q_name: String,
    pub q_type: QueryType,
    pub statement: SpannerStatement,
}

impl PreparedSpannerQuery {
    pub fn new(
        q_id: u16,
        q_name: String,
        q_type: QueryType,
        statement: SpannerStatement,
    ) -> Self {
        Self {
            q_id,
            q_name,
            q_type,
            statement,
        }
    }
}
