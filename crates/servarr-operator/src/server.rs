use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use prometheus::Encoder;
use tracing::info;

/// Shared state for the HTTP health/metrics server.
#[derive(Clone)]
pub struct ServerState {
    ready: Arc<AtomicBool>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the operator as ready (call after CRD registration).
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::Relaxed);
    }
}

/// Start the HTTP server on the given port.
///
/// Exposes:
/// - `GET /metrics` — Prometheus text format
/// - `GET /healthz` — liveness probe (always 200)
/// - `GET /readyz`  — readiness probe (200 after initial sync)
pub async fn run(port: u16, state: ServerState) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "starting metrics/health server");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn metrics_handler() -> impl IntoResponse {
    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    if encoder.encode(&metric_families, &mut buffer).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to encode metrics".to_string(),
        );
    }
    (
        StatusCode::OK,
        String::from_utf8(buffer).unwrap_or_default(),
    )
}

async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz_handler(State(state): State<ServerState>) -> impl IntoResponse {
    if state.ready.load(Ordering::Relaxed) {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}
