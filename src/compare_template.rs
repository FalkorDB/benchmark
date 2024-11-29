use crate::metrics_collector::Percentile;
use askama::Template;
use serde::Serialize;
use std::fmt;

#[derive(Template)]
#[template(path = "compare.html", escape = "none", ext = "html")]
pub struct CompareTemplate {
    pub data: CompareRuns,
}

#[derive(Serialize)]
pub struct CompareRuns {
    pub run_1: Percentile,
    pub run_2: Percentile,
}

impl fmt::Display for CompareRuns {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "{}", serde_json::to_string_pretty(&self).unwrap())
    }
}
