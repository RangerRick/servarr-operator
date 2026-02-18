mod context;
mod controller;
mod media_stack_controller;
mod metrics;
mod server;
mod telemetry;
mod webhook;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{error, info};

const METRICS_PORT: u16 = 8080;

#[derive(Parser)]
#[command(
    name = "servarr-operator",
    about = "Servarr Operator — Kubernetes operator for *arr media apps"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the ServarrApp CRD YAML to stdout.
    Crd,
}

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Crd) => {
            controller::print_crd()?;
            media_stack_controller::print_crd()?;
            return Ok(());
        }
        None => {}
    }

    let state = server::ServerState::new();

    // Optionally start the webhook server if WEBHOOK_ENABLED=true
    let webhook_enabled = std::env::var("WEBHOOK_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if webhook_enabled {
        let webhook_config = webhook::WebhookConfig::default();
        info!(port = webhook_config.port, "webhook server enabled");
        tokio::spawn(async move {
            if let Err(e) = webhook::run(webhook_config).await {
                error!(%e, "webhook server failed");
            }
        });
    }

    // Run the metrics/health server and both controllers concurrently.
    // If any exits, shut down.
    let state2 = state.clone();
    tokio::select! {
        res = server::run(METRICS_PORT, state.clone()) => {
            error!("metrics server exited: {res:?}");
            res
        }
        res = controller::run(state) => {
            res
        }
        res = media_stack_controller::run(state2) => {
            res
        }
    }
}
