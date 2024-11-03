#![allow(dead_code)]

use crate::error::BenchmarkResult;
use crate::foo::read_lines;
use crate::utils::{create_directory_if_not_exists, download_file, url_file_name};
use std::io;
use strum_macros::Display;
use tokio::fs;
use tracing::info;

#[derive(Debug, Clone, Display)]
pub enum Size {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, Display)]
pub enum Name {
    Users,
}

#[derive(Debug, Clone, Display)]
pub enum Vendor {
    Neo4j,
}
#[derive(Debug, Clone)]
pub struct Spec<'a> {
    pub(crate) name: Name,
    pub(crate) vendor: Vendor,
    pub(crate) size: Size,
    vertices: u64,
    edges: u64,
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
            (Name::Users, Size::Small) => Spec {
                name: Name::Users,
                size: Size::Small,
                vertices: 10000,
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
            (Name::Users, Size::Large)
            => Spec {
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

    pub(crate) fn backup_path(&self) -> String {
        format!("./backups/{}/{}/{}", self.vendor, self.name, self.size)
    }

    pub(crate) async fn init_data_iterator(&self) -> BenchmarkResult<impl Iterator<Item=io::Result<String>>> {
        let cached = self.cache(self.data_url.as_ref()).await?;
        info!("getting data from cache file {}", cached);
        read_lines(cached)
    }

    pub(crate) async fn cache(&self, url: &str) -> BenchmarkResult<String> {
        let file_name = url_file_name(url);
        let cache_dir = format!("./cache/{}/{}/{}", self.vendor, self.name, self.size);
        create_directory_if_not_exists(cache_dir.as_str()).await?;
        let cache_file = format!("{}/{}", cache_dir, file_name);
        // if cache_file not exists copy it from url
        if fs::metadata(cache_file.clone()).await.is_err() {
            info!("writing data from {} to file  {}", url, cache_file);
            download_file(url, cache_file.as_str()).await?;
        }
        Ok(cache_file)
    }
}
