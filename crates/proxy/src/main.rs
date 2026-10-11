use anyhow::Result;
use clap::Parser;
use provider::Protocol;
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

mod application;
#[path = "adapters/catalog.rs"]
mod catalog;
#[cfg(test)]
mod configuration_tests;
mod domain;
#[cfg(test)]
mod legacy_tests;
#[path = "adapters/management.rs"]
mod management;
#[path = "adapters/oauth.rs"]
mod oauth;
#[cfg(test)]
mod protocol_tests;
#[path = "adapters/upstream.rs"]
mod provider;
#[path = "adapters/anthropic.rs"]
mod server;
#[path = "adapters/sqlite.rs"]
mod sqlite;
#[path = "adapters/stream.rs"]
mod stream;
#[cfg(test)]
mod tests;
#[path = "adapters/translate.rs"]
mod translate;

struct Config {
    catalog: Option<Vec<serde_json::Value>>,
    #[cfg(test)]
    port: u16,
    timeout: Duration,
    providers: Vec<ProviderConfig>,
    client_version: String,
    project_id: String,
    protocols: HashMap<String, Protocol>,
}
struct ProviderConfig {
    opencode: bool,
    name: String,
    base: String,
    key: String,
    free_only: bool,
    models: Vec<String>,
}

#[derive(Parser)]
#[command(version, about = "Proxy Anthropic configurable desde una UI web")]
struct Args {
    #[arg(long, default_value = "claude-proxy.sqlite3")]
    database: PathBuf,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 10800)]
    port: u16,
    #[arg(long, default_value_t = 10801)]
    ui_port: u16,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    anyhow::ensure!(
        args.port > 0 && args.ui_port > 0 && args.port != args.ui_port,
        "Los dos puertos deben ser distintos y mayores que cero"
    );
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let app = Arc::new(application::Application::open(
        Arc::new(sqlite::SqliteStore::open(&args.database)?),
        args.port,
    )?);
    let gateway = tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await?;
    let admin = tokio::net::TcpListener::bind((args.host.as_str(), args.ui_port)).await?;
    tracing::info!(anthropic = %gateway.local_addr()?, ui = %admin.local_addr()?, "Proxy iniciado; configura los proveedores desde la UI");
    let (stop, _) = tokio::sync::watch::channel(false);
    let mut first = stop.subscribe();
    let mut second = stop.subscribe();
    let api =
        axum::serve(gateway, management::gateway(app.clone())).with_graceful_shutdown(async move {
            let _ = first.changed().await;
        });
    let ui = axum::serve(admin, management::router(app)).with_graceful_shutdown(async move {
        let _ = second.changed().await;
    });
    let signal = async move {
        shutdown().await;
        let _ = stop.send(true);
    };
    let (a, b, _) = tokio::join!(api, ui, signal);
    a?;
    b?;
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
