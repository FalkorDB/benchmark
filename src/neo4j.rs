use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::neo4j_client::Neo4jClient;
use crate::scenario::Spec;
use crate::utils::{create_directory_if_not_exists, spawn_command};
use std::env;
use std::process::Output;
use std::process::{Child, Command};
use std::time::Duration;
use tokio::{fs, time::sleep};
use tracing::{info, trace};

#[derive(Clone)]
pub struct Neo4j {
    uri: String,
    user: String,
    password: String,
    neo4j_home: String,
}

impl Default for Neo4j {
    fn default() -> Self {
        Self::new()
    }
}

impl Neo4j {
    fn new() -> Neo4j {
        let neo4j_home =
            env::var("NEO4J_HOME").unwrap_or_else(|_| String::from("./downloads/neo4j_local"));
        let uri = env::var("NEO4J_URI").unwrap_or_else(|_| String::from("127.0.0.1:7687"));
        let user = env::var("NEO4J_USER").unwrap_or_else(|_| String::from("neo4j"));
        let password = env::var("NEO4J_PASSWORD").unwrap_or_else(|_| String::from("h6u4krd10"));
        Neo4j {
            uri,
            user,
            password,
            neo4j_home,
        }
    }

    pub async fn restore_db<'a>(
        &self,
        spec: Spec<'a>,
    ) -> BenchmarkResult<Output> {
        self.restore(spec).await
    }

    pub async fn client(&self) -> BenchmarkResult<Neo4jClient> {
        Neo4jClient::new(
            self.uri.to_string(),
            self.user.to_string(),
            self.password.to_string(),
        )
        .await
    }

    fn neo4j_binary(&self) -> String {
        format!("{}/bin/neo4j", self.neo4j_home.clone())
    }

    fn neo4j_pid(&self) -> String {
        format!("{}/run/neo4j.pid", self.neo4j_home.clone())
    }

    fn neo4j_admin(&self) -> String {
        format!("{}/bin/neo4j-admin", self.neo4j_home.clone())
    }

    pub async fn dump<'a>(
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

    pub async fn restore<'a>(
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

    pub async fn clean_db(&self) -> BenchmarkResult<Output> {
        info!("cleaning DB");
        loop {
            if fs::metadata(&self.neo4j_pid()).await.is_ok() {
                info!("stopping neo4j before deleting the databases");
                self.stop(false).await?;
            }
            let home = self.neo4j_home.clone();
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
