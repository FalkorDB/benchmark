use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::neo4j_client::Neo4jClient;
use crate::scenario::Spec;
use crate::utils::{create_directory_if_not_exists, spawn_command};
use crate::{
    prometheus_metrics, CPU_USAGE_GAUGE, MEM_USAGE_GAUGE, NEO4J_CPU_USAGE_GAUGE,
    NEO4J_MEM_USAGE_GAUGE,
};
use std::env;
use std::ffi::OsString;
use std::process::Output;
use std::process::{Child, Command};
use std::time::Duration;
use sysinfo::{Pid, System};
use tokio::task::JoinHandle;
use tokio::{fs, time::sleep};
use tracing::{info, trace};

pub struct Neo4j {
    uri: String,
    user: String,
    password: String,
    neo4j_home: String,
    prom_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    prom_process_handle: Option<JoinHandle<()>>,
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
            prom_shutdown_tx: None,
            prom_process_handle: None,
        }
    }

    pub async fn restore_db(
        &self,
        spec: Spec<'_>,
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

    pub async fn dump(
        &self,
        spec: Spec<'_>,
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

    pub async fn restore(
        &self,
        spec: Spec<'_>,
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

    pub async fn clean_db(&mut self) -> BenchmarkResult<Output> {
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

    pub async fn start(&mut self) -> BenchmarkResult<Child> {
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

        let (prom_process_handle, prom_shutdown_tx) =
            prometheus_metrics::run_metrics_reporter(report_metrics);
        self.prom_process_handle = Some(prom_process_handle);
        self.prom_shutdown_tx = Some(prom_shutdown_tx);
        Ok(child)
    }

    pub async fn stop(
        &mut self,
        verbose: bool,
    ) -> BenchmarkResult<Output> {
        if verbose {
            info!("Stopping Neo4j process");
        }

        if let Some(prom_shutdown_tx) = self.prom_shutdown_tx.take() {
            drop(prom_shutdown_tx);
        }
        if let Some(prom_process_handle) = self.prom_process_handle.take() {
            let _ = prom_process_handle.await;
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

async fn report_metrics(sys: std::sync::Arc<std::sync::Mutex<System>>) -> BenchmarkResult<()> {
    let mut system = sys.lock().unwrap();
    // Refresh CPU usage
    system.refresh_all();
    let logical_cpus = system.cpus().len();
    let cpu_usage = system.global_cpu_usage() as i64 / logical_cpus as i64;
    CPU_USAGE_GAUGE.set(cpu_usage);

    // Refresh memory usage
    let mem_used = system.used_memory();
    MEM_USAGE_GAUGE.set(mem_used as i64);

    // Find the specific process
    if let Some(pid) = get_neo4j_server_pid() {
        if let Some(process) = system.process(Pid::from(pid as usize)) {
            let cpu_usage = process.cpu_usage() as i64 / logical_cpus as i64;
            NEO4J_CPU_USAGE_GAUGE.set(cpu_usage);
            let mem_used = process.memory() as i64;
            NEO4J_MEM_USAGE_GAUGE.set(mem_used);
        }
    }
    Ok(())
}

fn get_neo4j_server_pid() -> Option<u32> {
    let system = System::new_all();
    let res = system.processes().iter().find(|(_, process)| {
        search_in_os_strings(process.cmd(), "org.neo4j.server.CommunityEntryPoint")
    });
    res.map(|(pid, _)| pid.as_u32())
}

fn search_in_os_strings(
    os_strings: &[OsString],
    target: &str,
) -> bool {
    os_strings
        .iter()
        .any(|os_string| os_string.as_os_str().to_str() == Some(target))
}
