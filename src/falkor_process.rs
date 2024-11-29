use crate::error::BenchmarkError::FailedToSpawnProcessError;
use crate::error::BenchmarkResult;
use crate::utils::{create_directory_if_not_exists, falkor_shared_lib_path};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::env;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{error, info};

const REDIS_DATA_DIR: &str = "./redis-data";
pub struct FalkorProcess {
    state: Arc<Mutex<State>>,
}

struct State {
    child: Child,
    should_monitor: bool,
}

impl FalkorProcess {
    pub async fn new() -> BenchmarkResult<Self> {
        let child = Self::spawn_process().await?;
        let should_monitor = true;
        let state = State {
            child,
            should_monitor,
        };
        let state_arc = Arc::new(Mutex::new(state));
        let state_arc_clone = Arc::clone(&state_arc);
        Self::monitor_process(state_arc_clone);
        Ok(FalkorProcess { state: state_arc })
    }

    async fn spawn_process() -> BenchmarkResult<Child> {
        create_directory_if_not_exists(REDIS_DATA_DIR).await?;
        let default_so_path = falkor_shared_lib_path().unwrap();
        let default_so_path = env::var("FALKOR_PATH").unwrap_or_else(|_| default_so_path.clone());

        let command = "redis-server";
        let args = [
            "--dir",
            REDIS_DATA_DIR,
            "--loadmodule",
            default_so_path.as_str(),
            "CACHE_SIZE",
            "40",
        ];

        info!("starting falkor: {} {}", command, args.join(" "));

        let child = Command::new(command).args(&args).spawn().map_err(|e| {
            FailedToSpawnProcessError(
                e,
                format!(
                    "failed to spawn falkor process, cmd: {} {}",
                    command,
                    args.join(" ")
                )
                .to_string(),
            )
        })?;
        if let Some(pid) = child.id() {
            info!("falkor process started with PID: {}", pid);
        } else {
            error!("Failed to get PID of falkor process");
        }
        Ok(child)
    }
    fn monitor_process(arc_state: Arc<Mutex<State>>) {
        tokio::spawn(async move {
            loop {
                let mut state = arc_state.lock().await;

                // Check if monitoring is disabled
                if !state.should_monitor {
                    info!("Monitoring stopped; exiting loop.");
                    break;
                }
                let child_lock = &mut state.child;
                match child_lock.try_wait() {
                    Ok(Some(status)) => {
                        info!("falkor process exited with status: {:?}", status);
                        match Self::spawn_process().await {
                            Ok(new_child) => {
                                *child_lock = new_child;
                                info!("Restarted falkor process with PID: {:?}", child_lock.id());
                            }
                            Err(e) => {
                                error!("Failed to restart falkor process: {:?}", e);
                                break;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!("Failed to check falkor process status: {:?}", e);
                        break;
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });
    }

    pub async fn terminate(&mut self) -> BenchmarkResult<()> {
        info!("Terminating FalkorProcess");
        // Lock the state mutex to access the child process
        let mut state_lock = self.state.lock().await;

        // Set should_monitor to false to prevent restarting
        state_lock.should_monitor = false;

        if let Some(pid) = state_lock.child.id() {
            let pid = Pid::from_raw(pid as i32);

            // Send SIGTERM signal to request graceful termination
            match kill(pid, Signal::SIGTERM) {
                Ok(_) => {
                    info!("Sent SIGTERM signal to falkor process with PID: {}", pid);

                    // Optionally wait for the process to exit
                    if let Err(e) = tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        state_lock.child.wait(),
                    )
                    .await
                    {
                        error!(
                            "Timed out waiting for falkor process with PID: {} to exit: {:?}",
                            pid, e
                        );
                    } else {
                        info!("Falkor process with PID: {} exited successfully", pid);
                    }
                }
                Err(e) => error!(
                    "Failed to send SIGTERM signal to falkor process: {:?}, with PID: {}",
                    e, pid
                ),
            }
        } else {
            error!("Failed to get PID of falkor process");
        }

        Ok(())
    }
}

impl Drop for FalkorProcess {
    fn drop(&mut self) {
        info!("Dropping FalkorProcess");
        let rt = tokio::runtime::Runtime::new().unwrap();
        match rt.block_on(async { self.terminate().await }) {
            Ok(_) => {
                info!("FalkorProcess terminated successfully");
            }
            Err(_) => {
                error!("Failed to terminate FalkorProcess");
            }
        }
    }
}
