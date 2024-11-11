use crate::metrics_collector::Percentile;
use askama::Template;
use serde::Serialize;
use std::fmt;

#[derive(Template)]
#[template(path = "compare.html", escape = "none", ext = "html")]
pub(crate) struct CompareTemplate {
    pub(crate) data: CompareRuns,
}

#[derive(Serialize)]
pub(crate) struct CompareRuns {
    pub(crate) run_1: Percentile,
    pub(crate) run_2: Percentile,
}

impl fmt::Display for CompareRuns {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "{}", serde_json::to_string_pretty(&self).unwrap())
    }
}
