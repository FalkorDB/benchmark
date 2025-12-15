use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::memgraph_client::MemgraphClient;
use crate::scenario::Spec;
use crate::utils::{create_directory_if_not_exists, spawn_command};
use crate::{
    prometheus_metrics, CPU_USAGE_GAUGE, MEMGRAPH_CPU_USAGE_GAUGE, MEMGRAPH_MEM_USAGE_GAUGE,
    MEM_USAGE_GAUGE,
};
use std::env;
use std::ffi::OsString;
use std::process::Output;
use std::process::{Child, Command};
use std::time::Duration;
use sysinfo::{Pid, System};
use tokio::task::JoinHandle;
use tracing::{info, trace};

pub struct Memgraph {
    uri: String,
    user: String,
    password: String,
    memgraph_home: String,
    prom_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    prom_process_handle: Option<JoinHandle<()>>,
}

impl Default for Memgraph {
    fn default() -> Self {
        Self::new()
    }
}

impl Memgraph {
    fn new() -> Memgraph {
        let memgraph_home = env::var("MEMGRAPH_HOME")
            .unwrap_or_else(|_| String::from("./downloads/memgraph_local"));
        let uri = env::var("MEMGRAPH_URI").unwrap_or_else(|_| String::from("127.0.0.1:7687"));
        let user = env::var("MEMGRAPH_USER").unwrap_or_else(|_| String::from(""));
        let password = env::var("MEMGRAPH_PASSWORD").unwrap_or_else(|_| String::from(""));
        Memgraph {
            uri,
            user,
            password,
            memgraph_home,
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

    pub async fn client(&self) -> BenchmarkResult<MemgraphClient> {
        MemgraphClient::new(
            self.uri.to_string(),
            self.user.to_string(),
            self.password.to_string(),
        )
        .await
    }

    fn memgraph_binary(&self) -> String {
        format!("{}/memgraph", self.memgraph_home.clone())
    }

    #[allow(dead_code)]
    fn memgraph_pid_file(&self) -> String {
        format!("{}/memgraph.pid", self.memgraph_home.clone())
    }

    pub async fn dump(
        &self,
        spec: Spec<'_>,
    ) -> BenchmarkResult<Output> {
        if self.is_running().await? {
            return Err(OtherError(
                "Cannot dump DB because it is running.".to_string(),
            ));
        }
        let backup_path = spec.backup_path();

        info!("Dumping DB to {}", backup_path);
        create_directory_if_not_exists(&backup_path).await?;

        // Memgraph doesn't have a built-in dump command like Neo4j
        // We'll use a cypher script to export the data
        let dump_file = format!("{}/memgraph.cypher", backup_path);

        // Start memgraph temporarily for dump
        let mut temp_process = self.start_temp_for_dump().await?;

        let client = self.client().await?;
        client.export_to_file(&dump_file).await?;

        // Stop the temporary process
        temp_process.kill()?;
        temp_process.wait()?;

        Ok(Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    async fn start_temp_for_dump(&self) -> BenchmarkResult<Child> {
        trace!("starting temporary Memgraph process for dump");
        let child = Command::new(self.memgraph_binary())
            .arg("--data-directory")
            .arg(format!("{}/data", self.memgraph_home))
            .arg("--storage-wal-enabled=false")
            .arg("--storage-snapshot-interval-sec=0")
            .spawn()
            .map_err(|e| {
                FailedToSpawnProcessError(
                    e,
                    format!(
                        "Failed to spawn temporary Memgraph process, cmd: {}",
                        self.memgraph_binary()
                    ),
                )
            })?;

        // Wait for Memgraph to be ready
        tokio::time::sleep(Duration::from_secs(3)).await;
        Ok(child)
    }

    pub async fn restore(
        &self,
        spec: Spec<'_>,
    ) -> BenchmarkResult<Output> {
        info!("Restoring DB");
        if self.is_running().await? {
            return Err(OtherError(
                "Cannot restore DB because it is running.".to_string(),
            ));
        }

        let backup_path = spec.backup_path();
        let dump_file = format!("{}/memgraph.cypher", backup_path);

        // Start memgraph temporarily for restore
        let mut temp_process = self.start_temp_for_dump().await?;

        let client = self.client().await?;
        client.import_from_file(&dump_file).await?;

        // Stop the temporary process
        temp_process.kill()?;
        temp_process.wait()?;

        Ok(Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    pub async fn clean_db(&mut self) -> BenchmarkResult<Output> {
        info!("cleaning DB");

        if self.is_running().await? {
            info!("stopping memgraph before deleting the data");
            self.stop(false).await?;
        }

        let home = self.memgraph_home.clone();
        let data_dir = format!("{}/data", &home);
        let log_dir = format!("{}/log", &home);

        let args = ["-rf", &data_dir, &log_dir];
        spawn_command("rm", &args).await
    }

    pub async fn start(&mut self) -> BenchmarkResult<Child> {
        trace!("starting Memgraph process: {}", self.memgraph_binary());

        if self.is_running().await? {
            self.stop(false).await?;
        }

        info!("starting Memgraph process");

        // Create data directory if it doesn't exist
        let data_dir = format!("{}/data", self.memgraph_home);
        create_directory_if_not_exists(&data_dir).await?;

        let child = Command::new(self.memgraph_binary())
            .arg("--data-directory")
            .arg(&data_dir)
            .arg("--log-level=WARNING")
            .arg("--also-log-to-stderr=false")
            .spawn()
            .map_err(|e| {
                FailedToSpawnProcessError(
                    e,
                    format!(
                        "Failed to spawn Memgraph process, cmd: {}",
                        self.memgraph_binary()
                    ),
                )
            })?;

        // Wait for Memgraph to be ready
        let mut retries = 0;
        let max_retries = 30;
        while !self.is_running().await? && retries < max_retries {
            tokio::time::sleep(Duration::from_secs(1)).await;
            retries += 1;
        }

        if retries >= max_retries {
            return Err(OtherError(
                "Memgraph failed to start within timeout".to_string(),
            ));
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
        let pid: u32 = child.id();

        info!("Memgraph is running: {}", pid);

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
            info!("Stopping Memgraph process");
        }

        if let Some(prom_shutdown_tx) = self.prom_shutdown_tx.take() {
            drop(prom_shutdown_tx);
        }
        if let Some(prom_process_handle) = self.prom_process_handle.take() {
            let _ = prom_process_handle.await;
        }

        // Memgraph doesn't have a dedicated stop command, so we'll kill the process
        if let Some(pid) = get_memgraph_server_pid() {
            let args = ["-TERM", &pid.to_string()];
            spawn_command("kill", &args).await
        } else {
            Ok(Output {
                status: std::process::ExitStatus::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    pub async fn is_running(&self) -> BenchmarkResult<bool> {
        Ok(get_memgraph_server_pid().is_some())
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
    if let Some(pid) = get_memgraph_server_pid() {
        if let Some(process) = system.process(Pid::from(pid as usize)) {
            let cpu_usage = process.cpu_usage() as i64 / logical_cpus as i64;
            MEMGRAPH_CPU_USAGE_GAUGE.set(cpu_usage);
            let mem_used = process.memory() as i64;
            MEMGRAPH_MEM_USAGE_GAUGE.set(mem_used);
        }
    }
    Ok(())
}

fn get_memgraph_server_pid() -> Option<u32> {
    let system = System::new_all();
    let res = system.processes().iter().find(|(_, process)| {
        search_in_os_strings(process.cmd(), "memgraph") || process.name() == "memgraph"
    });
    res.map(|(pid, _)| pid.as_u32())
}

fn search_in_os_strings(
    os_strings: &[OsString],
    target: &str,
) -> bool {
    os_strings.iter().any(|os_string| {
        if let Some(s) = os_string.as_os_str().to_str() {
            s.contains(target)
        } else {
            false
        }
    })
}
