use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use prometheus::{Encoder, TextEncoder};
use std::net::SocketAddr;
use tokio::sync::oneshot::Sender;
use tokio::task;
use tokio::task::JoinHandle;
use tracing::{error, info, trace};

pub struct PrometheusEndpoint {
    shutdown_tx: Option<Sender<()>>,
    server_thread: Option<JoinHandle<()>>,
}

impl Default for PrometheusEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl PrometheusEndpoint {
    fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let server_thread = task::spawn(async {
            let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

            let make_svc = make_service_fn(|_conn| async {
                Ok::<_, hyper::Error>(service_fn(metrics_handler))
            });

            let server = Server::bind(&addr).serve(make_svc);
            let graceful = server.with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            });
            info!("Listening on http://{}", addr);

            if let Err(e) = graceful.await {
                error!("server error: {}", e);
            }
        });

        PrometheusEndpoint {
            shutdown_tx: Some(shutdown_tx),
            server_thread: Some(server_thread),
        }
    }
}

impl Drop for PrometheusEndpoint {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            info!("Prometheus endpoint shutdown");
        }
        if let Some(thread) = self.server_thread.take() {
            drop(thread);
        }
    }
}

async fn metrics_handler(_req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    trace!("Metrics request received");
    Ok(Response::builder()
        .header(hyper::header::CONTENT_TYPE, encoder.format_type())
        .body(Body::from(buffer))
        .unwrap())
}
