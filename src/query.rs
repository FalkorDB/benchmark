// CYPHER name_param = "Niccolò Machiavelli" birth_year_param = 1469 MATCH (p:Person {name: $name_param, birth_year: $birth_year_param}) RETURN p
use neo4rs::BoltType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub params: HashMap<String, QueryParam>,
}

impl Query {
    pub fn to_cypher(&self) -> String {
        let mut param_strings: Vec<String> = self
            .params
            .iter()
            .map(|(k, v)| format!("{} = {}", k, v.to_cypher_string()))
            .collect();
        param_strings.sort();
        let params_str = param_strings.join(" ");
        format!("CYPHER {} {}", params_str, self.text)
    }

    pub fn to_bolt(&self) -> (String, Vec<(String, QueryParam)>) {
        let query = self.text.clone();
        let params: Vec<(String, QueryParam)> = self
            .params
            .clone()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        (query, params)
    }
    pub fn to_bolt_struct(&self) -> Bolt {
        let (query, params) = self.to_bolt();
        Bolt { query, params }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Bolt {
    pub query: String,
    pub params: Vec<(String, QueryParam)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QueryParam {
    String(String),
    Integer(i32),
    Float(f32),
    Boolean(bool),
}

impl From<QueryParam> for BoltType {
    fn from(param: QueryParam) -> BoltType {
        match param {
            QueryParam::String(s) => s.into(),
            QueryParam::Integer(i) => i.into(),
            QueryParam::Float(f) => f.into(),
            QueryParam::Boolean(b) => b.into(),
        }
    }
}

impl QueryParam {
    pub fn to_cypher_string(&self) -> String {
        match self {
            QueryParam::String(s) => format!("\"{}\"", s.replace("\"", "\\\"")),
            QueryParam::Integer(i) => i.to_string(),
            QueryParam::Float(f) => f.to_string(),
            QueryParam::Boolean(b) => b.to_string(),
        }
    }
}
impl PartialEq for QueryParam {
    fn eq(
        &self,
        other: &Self,
    ) -> bool {
        match (self, other) {
            (QueryParam::String(a), QueryParam::String(b)) => a == b,
            (QueryParam::Integer(a), QueryParam::Integer(b)) => a == b,
            (QueryParam::Float(a), QueryParam::Float(b)) => a.to_bits() == b.to_bits(),
            (QueryParam::Boolean(a), QueryParam::Boolean(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for QueryParam {}

#[derive(Debug, Default, Clone)]
pub struct QueryBuilder {
    query: Query,
}

impl QueryBuilder {
    pub fn new() -> Self {
        QueryBuilder::default()
    }

    pub fn text(
        mut self,
        text: impl Into<String>,
    ) -> Self {
        self.query.text = text.into();
        self
    }

    pub fn param<T: Into<String>, V: Into<QueryParam>>(
        mut self,
        key: T,
        value: V,
    ) -> Self {
        self.query.params.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> Query {
        self.query
    }
}

impl From<String> for QueryParam {
    fn from(value: String) -> Self {
        QueryParam::String(value)
    }
}
impl From<&str> for QueryParam {
    fn from(value: &str) -> Self {
        QueryParam::String(value.to_string())
    }
}

impl From<i32> for QueryParam {
    fn from(value: i32) -> Self {
        QueryParam::Integer(value)
    }
}

impl From<f32> for QueryParam {
    fn from(value: f32) -> Self {
        QueryParam::Float(value)
    }
}

impl From<bool> for QueryParam {
    fn from(value: bool) -> Self {
        QueryParam::Boolean(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_builder() {
        let query = QueryBuilder::new()
            .text("MATCH (p:Person {name: $name, birth_year: $birth_year}) RETURN p")
            .param("name", "Niccolò Machiavelli")
            .param("birth_year", 1469)
            .build();

        assert_eq!(
            query.text,
            "MATCH (p:Person {name: $name, birth_year: $birth_year}) RETURN p"
        );
        assert_eq!(query.params.len(), 2);
        assert!(
            matches!(query.params.get("name"), Some(QueryParam::String(s)) if s == "Niccolò Machiavelli")
        );
        assert!(
            matches!(query.params.get("birth_year"), Some(QueryParam::Integer(i)) if *i == 1469)
        );
    }

    #[test]
    fn test_query_to_cypher() {
        let query = QueryBuilder::new()
            .text("MATCH (p:Person {name: $name, birth_year: $birth_year}) RETURN p")
            .param("name", "Niccolò Machiavelli")
            .param("birth_year", 1469)
            .build();

        let cypher = query.to_cypher();
        assert!(cypher.starts_with("CYPHER "));
        assert!(cypher.contains("birth_year = 1469"));
        assert!(cypher.contains("name = \"Niccolò Machiavelli\""));
        assert!(
            cypher.ends_with("MATCH (p:Person {name: $name, birth_year: $birth_year}) RETURN p")
        );
    }

    #[test]
    fn test_query_param_to_cypher_string() {
        assert_eq!(
            QueryParam::String("test".to_string()).to_cypher_string(),
            "\"test\""
        );
        assert_eq!(QueryParam::Integer(42).to_cypher_string(), "42");
        assert_eq!(QueryParam::Float(3.16).to_cypher_string(), "3.16");
        assert_eq!(QueryParam::Boolean(true).to_cypher_string(), "true");
    }

    #[test]
    fn test_query_param_from_impls() {
        assert!(matches!(QueryParam::from("test"), QueryParam::String(s) if s == "test"));
        assert!(
            matches!(QueryParam::from("test".to_string()), QueryParam::String(s) if s == "test")
        );
        assert!(matches!(QueryParam::from(42), QueryParam::Integer(i) if i == 42));
        assert!(matches!(QueryParam::from(3.16), QueryParam::Float(f) if f == 3.16));
        assert!(matches!(QueryParam::from(true), QueryParam::Boolean(b) if b));
    }
}
