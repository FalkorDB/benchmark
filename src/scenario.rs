#![allow(dead_code)]

use crate::error::BenchmarkResult;
use crate::line_stream::LinesStreamResponse;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::TryStreamExt;
use reqwest_streams::error::{StreamBodyError, StreamBodyKind};
use reqwest_streams::StreamBodyResult;
use tokio_util::io::StreamReader;
use tracing::info;

#[derive(Debug, Clone)]
pub enum Size {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone)]
pub enum Name {
    Pokec,
}

#[derive(Debug, Clone)]
pub enum Vendor {
    Neo4j,
}
#[derive(Debug, Clone)]
pub struct Spec<'a> {
    name: Name,
    size: Size,
    vertices: u64,
    edges: u64,
    vendor: Vendor,
    data_url: &'a str,
    index_url: &'a str,
}

impl<'a> Spec<'a> {
    pub fn new(
        name: Name,
        size: Size,
        vendor: Vendor,
    ) -> Self {
        match (name, size) {
            (Name::Pokec, Size::Small) => Spec {
                name: Name::Pokec,
                size: Size::Small,
                vertices: 10000,
                edges: 121716,
                vendor,
                data_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/pokec_small_import.cypher",
                index_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/neo4j.cypher",
            },
            (Name::Pokec, Size::Medium) => Spec {
                name: Name::Pokec,
                size: Size::Medium,
                vertices: 100000,
                edges: 1768515,
                vendor,
                data_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/pokec_medium_import.cypher",
                index_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/neo4j.cypher",
            },
            (Name::Pokec, Size::Large)
            => Spec {
                name: Name::Pokec,
                size: Size::Large,
                vertices: 1632803,
                edges: 30622564,
                vendor,
                data_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/pokec_large.setup.cypher.gz",
                index_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/neo4j.cypher",
            },
        }
    }

    pub(crate) async fn stream_data<'b>(
        &self
    ) -> BenchmarkResult<BoxStream<'b, StreamBodyResult<String>>> {
        stream_lines(self.data_url).await
    }
}

async fn stream_lines<'b>(url: &str) -> BenchmarkResult<BoxStream<'b, StreamBodyResult<String>>> {
    info!("streaming lines from {}", url);
    let response = reqwest::get(url).await?;
    Ok(response.lines_stream(1024 * 10))
}
