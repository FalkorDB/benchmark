use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::scenario::Spec;
use crate::utils::{create_directory_if_not_exists, spawn_command};
use std::process::{Child, Command, Output};
use std::time::Duration;
use tokio::{fs, time::sleep};
use tracing::{info, trace};

#[derive(Debug, Clone)]
pub struct Neo4jProcess {
    home: String,
}

impl Neo4jProcess {
    pub fn new(home: String) -> Neo4jProcess {
        Neo4jProcess { home }
    }

    fn neo4j_binary(&self) -> String {
        format!("{}/bin/neo4j", self.home.clone())
    }

    fn neo4j_pid(&self) -> String {
        format!("{}/run/neo4j.pid", self.home.clone())
    }

    fn neo4j_admin(&self) -> String {
        format!("{}/bin/neo4j-admin", self.home.clone())
    }

    pub(crate) async fn dump<'a>(
        &self,
        spec: Spec<'a>,
    ) -> BenchmarkResult<Output> {
        if fs::metadata(&self.neo4j_pid()).await.is_ok() {
            return Err(OtherError(
                "Cannot dump DB because it is running.".to_string(),
            ));
        }
        let backup_path = spec.backup_path();

        let command = self.neo4j_admin();
        info!("Dumping DB to {}", backup_path);
        create_directory_if_not_exists(&backup_path).await?;

        let args = [
            "database",
            "dump",
            "--overwrite-destination=true",
            "--to-path",
            backup_path.as_str(),
            "neo4j",
        ];
        spawn_command(command.as_str(), &args).await
    }

    pub(crate) async fn restore<'a>(
        &self,
        spec: Spec<'a>,
    ) -> BenchmarkResult<Output> {
        info!("Restoring DB");
        if fs::metadata(&self.neo4j_pid()).await.is_ok() {
            return Err(OtherError(
                "Cannot restore DB because it is running.".to_string(),
            ));
        }
        let command = self.neo4j_admin();
        let backup_path = spec.backup_path();

        let args = [
            "database",
            "load",
            "--from-path",
            backup_path.as_str(),
            "--overwrite-destination=true",
            "neo4j",
        ];
        spawn_command(command.as_str(), &args).await
    }

    pub(crate) async fn clean_db(&self) -> BenchmarkResult<Output> {
        info!("cleaning DB");
        loop {
            if fs::metadata(&self.neo4j_pid()).await.is_ok() {
                info!("stopping neo4j before deleting the databases");
                self.stop(false).await?;
            }
            let home = self.home.clone();
            let databases = format!("{}/data/databases/neo4j", &home);
            let transactions = format!("{}/data/transactions/neo4j", &home);
            let args = ["-rf", &databases, &transactions];
            let out = spawn_command("rm", &args).await?;
            if out.status.success() {
                return Ok(out);
            } else {
                match (
                    fs::metadata(databases).await,
                    fs::metadata(transactions).await,
                ) {
                    (Err(_), Err(_)) => {
                        return Ok(out);
                    }
                    _ => {
                        sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                }
            }
        }
    }

    pub async fn start(&self) -> BenchmarkResult<Child> {
        trace!("starting Neo4j process: {} console", self.neo4j_binary());
        if fs::metadata(&self.neo4j_pid()).await.is_ok() {
            self.stop(false).await?;
        }
        info!("starting Neo4j process");
        let child = Command::new(self.neo4j_binary())
            .arg("console")
            // .stdout(Stdio::null()) // Redirect stdout to null
            .spawn()
            .map_err(|e| {
                FailedToSpawnProcessError(
                    e,
                    format!(
                        "Failed to spawn Neo4j process, cmd: {} console",
                        self.neo4j_binary()
                    )
                    .to_string(),
                )
            })?;

        while !self.is_running().await? {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
        let pid: u32 = child.id();

        info!("Neo4j is running: {}", pid);
        Ok(child)
    }

    pub async fn stop(
        &self,
        verbose: bool,
    ) -> BenchmarkResult<Output> {
        if verbose {
            info!("Stopping Neo4j process");
        }

        let command = self.neo4j_binary();

        let args = ["stop"];

        spawn_command(command.as_str(), &args).await
    }

    pub async fn is_running(&self) -> BenchmarkResult<bool> {
        trace!("Starting Neo4j process: {} status", self.neo4j_binary());
        let command = self.neo4j_binary();

        let args = ["status"];

        let output = spawn_command(command.as_str(), &args).await;
        match output {
            Ok(output) => {
                if output.status.success() {
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(_) => Ok(false),
        }
    }
}
