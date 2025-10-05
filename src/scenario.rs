#![allow(dead_code)]

use crate::error::BenchmarkResult;
use crate::utils::{create_directory_if_not_exists, download_file, read_lines, url_file_name};
use clap::ValueEnum;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::pin::Pin;
use strum_macros::Display;
use tracing::info;

#[derive(
    Debug, Clone, Display, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Serialize, Deserialize,
)]
#[strum(serialize_all = "lowercase")]
pub enum Size {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, Display, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[strum(serialize_all = "lowercase")]
pub enum Name {
    Users,
}

#[derive(Debug, Clone, Display, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[strum(serialize_all = "lowercase")]
pub enum Vendor {
    Neo4j,
    Falkor,
    Memgraph,
}

#[derive(Debug, Clone)]
pub struct Spec<'a> {
    pub name: Name,
    pub vendor: Vendor,
    pub size: Size,
    pub vertices: u64,
    pub edges: u64,
    data_url: &'a str,
    index_url: &'a str,
}

impl Spec<'_> {
    pub fn new(
        name: Name,
        size: Size,
        vendor: Vendor,
    ) -> Self {
        match (name, size) {
            (Name::Users, Size::Small) => Spec {
                name: Name::Users,
                size: Size::Small,
                vertices: 10000, // max user id 9998 min user id 1
                edges: 121716,
                vendor,
                data_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/pokec_small_import.cypher",
                index_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/neo4j.cypher",
            },
            (Name::Users, Size::Medium) => Spec {
                name: Name::Users,
                size: Size::Medium,
                vertices: 100000,
                edges: 1768515,
                vendor,
                data_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/pokec_medium_import.cypher",
                index_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/neo4j.cypher",
            },
            (Name::Users, Size::Large) => Spec {
                name: Name::Users,
                size: Size::Large,
                vertices: 1632803,
                edges: 30622564,
                vendor,
                data_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/pokec_large.setup.cypher.gz",
                index_url: "https://s3.eu-west-1.amazonaws.com/deps.memgraph.io/dataset/pokec/benchmark/neo4j.cypher",
            },
        }
    }

    pub fn backup_path(&self) -> String {
        format!("./backups/{}/{}/{}", self.vendor, self.name, self.size)
    }

    pub async fn init_data_iterator(
        &self
    ) -> BenchmarkResult<Pin<Box<dyn Stream<Item = io::Result<String>> + Send>>> {
        let cached = self.cache(self.data_url.as_ref()).await?;
        info!("Loading data from cache file {}", cached);
        Ok(Box::pin(read_lines(cached).await?))
    }
    pub async fn init_index_iterator(
        &self
    ) -> BenchmarkResult<Pin<Box<dyn Stream<Item = io::Result<String>> + Send>>> {
        let cached = self.cache(self.index_url.as_ref()).await?;
        info!("Loading indexes from cache file {}", cached);
        Ok(Box::pin(read_lines(cached).await?))
    }

    pub async fn cache(
        &self,
        url: &str,
    ) -> BenchmarkResult<String> {
        let file_name = url_file_name(url);
        let cache_dir = format!("./cache/{}/{}/{}", self.vendor, self.name, self.size);
        create_directory_if_not_exists(cache_dir.as_str()).await?;
        let cache_file = format!("{}/{}", cache_dir, file_name);
        // if cache_file not exists copy it from url
        if fs::metadata(cache_file.clone()).is_err() {
            info!(
                "Downloading data from {} to a cache file {}",
                url, cache_file
            );
            download_file(url, cache_file.as_str()).await?;
        }
        Ok(cache_file)
    }
}
