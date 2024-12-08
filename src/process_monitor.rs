use crate::error::BenchmarkResult;
use prometheus::core::{AtomicU64, GenericCounter};
use std::collections::HashMap;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout, Duration};
use tracing::{error, info, warn};

pub struct ProcessMonitor {
    command: String,
    args: Vec<String>,
    env_vars: HashMap<String, String>,
    shutdown_signal: oneshot::Receiver<()>,
    grace_period: Duration,
}

impl ProcessMonitor {
    pub fn new(
        command: String,
        args: Vec<String>,
        env_vars: HashMap<String, String>,
        grace_period: Duration,
    ) -> (Self, oneshot::Sender<()>) {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let monitor = Self {
            command,
            args,
            env_vars,
            shutdown_signal: shutdown_rx,
            grace_period,
        };

        (monitor, shutdown_tx)
    }

    pub async fn run(
        &mut self,
        restarts_counter: GenericCounter<AtomicU64>,
    ) -> BenchmarkResult<()> {
        restarts_counter.reset();
        loop {
            let mut child = self.spawn_process().await?;
            info!("Process spawned with PID: {:?}", child.id());
            // wait 10 seconds for the process to start
            // sleep(Duration::from_secs(10)).await;

            tokio::select! {
                status = child.wait() => {
                    match status {
                        Ok(status) => {
                            warn!("Process exited with status: {:?}", status);
                            restarts_counter.inc();
                            sleep(Duration::from_secs(1)).await;
                        }
                        Err(e) => {
                            error!("Error waiting for process: {:?}", e);
                            sleep(Duration::from_secs(5)).await;
                        }
                    }
                }

                _ = &mut self.shutdown_signal => {
                    info!("Shutting down process monitor");
                    self.terminate_process(&mut child).await;
                    return Ok(());
                }
            }
        }
    }

    async fn spawn_process(&self) -> BenchmarkResult<Child> {
        let mut command = Command::new(&self.command);
        command.args(&self.args);

        info!(
            "Spawning process: {} {}",
            &self.command,
            &self.args.join(" ")
        );

        // Add environment variables
        for (key, value) in &self.env_vars {
            command.env(key, value);
        }

        Ok(command.spawn()?)
    }

    async fn terminate_process(
        &self,
        child: &mut Child,
    ) {
        // Send SIGTERM
        if let Err(e) = child.start_kill() {
            error!("Failed to send SIGTERM: {:?}", e);
            return;
        }

        // Wait for the process to exit with a timeout
        match timeout(self.grace_period, child.wait()).await {
            Ok(Ok(_)) => info!("Process terminated gracefully"),
            Ok(Err(e)) => error!("Error waiting for process to terminate: {:?}", e),
            Err(_) => {
                // Timeout occurred, force kill
                error!("Process didn't terminate within grace period, forcing kill");
                if let Err(e) = child.kill().await {
                    error!("Failed to kill process: {:?}", e);
                }
            }
        }
    }
}
