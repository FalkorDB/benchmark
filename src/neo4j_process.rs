use crate::error::BenchmarkError::FailedToSpawnNeo4jError;
use crate::error::BenchmarkResult;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio::time::{sleep, Duration};
use tracing::{info, trace};

#[derive(Debug, Clone)]
pub struct Neo4jProcess {
    path: String,
}

impl Neo4jProcess {}

impl Neo4jProcess {
    pub fn new(path: String) -> Neo4jProcess {
        Neo4jProcess { path }
    }

    pub async fn start(&self) -> BenchmarkResult<Child> {
        trace!("Starting Neo4j process: {} console", self.path);
        info!("Starting Neo4j process");
        let child = Command::new(self.path.clone())
            .arg("console")
            .stdout(Stdio::null()) // Redirect stdout to null
            .spawn()
            .map_err(|e| {
                FailedToSpawnNeo4jError(
                    e,
                    format!("Failed to spawn Neo4j process, cmd: {} console", self.path)
                        .to_string(),
                )
            })?;

        while !self.is_running().await? {
            sleep(Duration::from_secs(1)).await;
        }
        let pid: Option<u32> = child.id();

        trace!("Neo4j is running: {:?}", pid);
        Ok(child)
    }

    pub async fn stop(&self) -> BenchmarkResult<()> {
        trace!("Starting Neo4j process: {} stop", self.path);
        info!("Stopping Neo4j process");
        Command::new(self.path.clone())
            .arg("stop")
            .output()
            .await
            .map_err(|e| {
                FailedToSpawnNeo4jError(
                    e,
                    format!("Failed to spawn Neo4j process, path: {} stop", self.path).to_string(),
                )
            })?;
        Ok(())
    }

    pub async fn is_running(&self) -> BenchmarkResult<bool> {
        trace!("Starting Neo4j process: {} status", self.path);
        let out = Command::new(self.path.clone())
            .arg("status")
            .output()
            .await
            .map_err(|e| {
                FailedToSpawnNeo4jError(
                    e,
                    format!("Failed to spawn Neo4j process, path: {} status", self.path)
                        .to_string(),
                )
            })?;
        if String::from_utf8_lossy(&out.stderr)
            .trim()
            .contains("Neo4j is not running.")
        {
            Ok(false)
        } else {
            Ok(true)
        }
    }
}
