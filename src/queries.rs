// CYPHER name_param = "Niccolò Machiavelli" birth_year_param = 1469 MATCH (p:Person {name: $name_param, birth_year: $birth_year_param}) RETURN p
use serde_json::json;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct Query {
    text: String,
    params: HashMap<String, QueryParam>,
}
impl Query {
    pub fn to_cypher(&self) -> String {
        let mut param_strings: Vec<String> = self
            .params
            .iter()
            .map(|(k, v)| format!("{} = {}", k, v.to_cypher_string()))
            .collect();
        param_strings.sort();
        let params_str = param_strings.join(", ");
        format!("CYPHER {} {}", params_str, self.text)
    }
    pub fn to_bolt(&self) -> (String, String) {
        let query = self.text.clone();
        let params = json!(self
            .params
            .iter()
            .map(|(k, v)| (k, v.to_json()))
            .collect::<HashMap<_, _>>());
        (query, params.to_string())
    }
}

#[derive(Debug, Clone)]
pub enum QueryParam {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

impl QueryParam {
    fn to_cypher_string(&self) -> String {
        match self {
            QueryParam::String(s) => format!("\"{}\"", s.replace("\"", "\\\"")),
            QueryParam::Integer(i) => i.to_string(),
            QueryParam::Float(f) => f.to_string(),
            QueryParam::Boolean(b) => b.to_string(),
        }
    }
    fn to_json(&self) -> serde_json::Value {
        match self {
            QueryParam::String(s) => json!(s),
            QueryParam::Integer(i) => json!(i),
            QueryParam::Float(f) => json!(f),
            QueryParam::Boolean(b) => json!(b),
        }
    }
}

#[derive(Debug, Default)]
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

impl From<i64> for QueryParam {
    fn from(value: i64) -> Self {
        QueryParam::Integer(value)
    }
}

impl From<f64> for QueryParam {
    fn from(value: f64) -> Self {
        QueryParam::Float(value)
    }
}

impl From<bool> for QueryParam {
    fn from(value: bool) -> Self {
        QueryParam::Boolean(value)
    }
}

fn main() {
    let query = QueryBuilder::new()
        .text("MATCH (p:Person {name: $name, birth_year: $birth_year}) RETURN p")
        .param("name", "Niccolò Machiavelli")
        .param("birth_year", 1469i64)
        .build();

    println!("{:#?}", query.to_cypher());
}
