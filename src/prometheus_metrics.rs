use crate::error::BenchmarkResult;
use std::future::Future;
use std::sync::{Arc, Mutex};
use sysinfo::System;
use tokio::task::JoinHandle;
use tracing::info;

pub(crate) fn run_metrics_reporter<FN, FUTURE>(
    measure: FN
) -> (JoinHandle<()>, tokio::sync::oneshot::Sender<()>)
where
    FN: Fn(Arc<Mutex<System>>) -> FUTURE + Send + Sync + 'static,
    FUTURE: Future<Output = BenchmarkResult<()>> + Send + 'static,
{
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

    let handle = tokio::spawn(async move {
        let system = Arc::new(Mutex::new(System::new_all()));
        let sys = system.clone();
        if let Err(e) = measure(sys).await {
            info!("Error reporting metrics: {:?}", e);
        }

        loop {
            let sys = system.clone();
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                    if let Err(e) = measure(sys).await {
                        info!("Error reporting metrics: {:?}", e);
                    }
                }
                _ = &mut shutdown_rx => {
                    // info!("Shutting down prometheus_metrics_reporter");
                    return;
                }
            }
        }
    });

    (handle, shutdown_tx)
}
