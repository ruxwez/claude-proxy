use anyhow::Result;
use clap::Parser;
use provider::Protocol;
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

mod application;
#[cfg(test)]
mod architecture_tests;
mod auth;
#[path = "adapters/catalog.rs"]
mod catalog;
#[cfg(test)]
mod configuration_tests;
#[path = "adapters/database.rs"]
mod database;
#[cfg(test)]
mod diagnostics_tests;
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
#[path = "adapters/traces.rs"]
mod traces;
#[path = "adapters/zed.rs"]
mod zed;
#[cfg(test)]
mod zed_tests;
use claude_proxy_engine::translate;

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
    zed: Option<zed::Config>,
    opencode: bool,
    name: String,
    base: String,
    key: String,
    free_only: bool,
    models: Vec<String>,
}

#[derive(Parser)]
#[command(version, about = "Proxy Anthropic administrado desde claude-proxy")]
struct Args {
    #[command(subcommand)]
    service: Option<claude_proxy_service::Command>,
    #[arg(long)]
    database: Option<PathBuf>,
    /// Directorio de infraestructura del servidor
    #[arg(long, env = "PROXY_CONFIG_DIR")]
    config_dir: Option<PathBuf>,
    #[arg(long, env = "PROXY_HOST", default_value = "127.0.0.1")]
    host: String,
    /// Dominio DNS de administración permitido; repetible
    #[arg(long, env = "PROXY_ADMIN_HOST", value_delimiter = ',')]
    admin_host: Vec<String>,
    #[arg(long, env = "PROXY_PORT", default_value_t = 10800)]
    port: u16,
    #[arg(
        long,
        env = "PROXY_ADMIN_PORT",
        alias = "ui-port",
        default_value_t = 10801
    )]
    admin_port: u16,
    #[arg(long, hide = true)]
    windows_service: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(command) = args.service {
        return claude_proxy_service::execute(command, None);
    }
    if args.windows_service {
        return claude_proxy_service::dispatch(run_server);
    }
    run_server()
}
fn run_server() -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(serve(Args::parse()))
}
async fn serve(args: Args) -> Result<()> {
    anyhow::ensure!(
        args.port > 0 && args.admin_port > 0 && args.port != args.admin_port,
        "Los dos puertos deben ser distintos y mayores que cero"
    );
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let directory = args.config_dir.unwrap_or_else(|| {
        directories::ProjectDirs::from("", "", "claude-proxy")
            .expect("Directorio de configuración no disponible")
            .config_dir()
            .join("server")
    });
    std::fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => Some(url),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => anyhow::bail!("DATABASE_URL debe ser UTF-8"),
    };
    if let Ok(url) = std::env::var("PROXY_PUBLIC_URL") {
        application::validate_url(&url)?;
    }
    let bootstrap = directory.join("database.json");
    let legacy = PathBuf::from("claude-proxy.sqlite3");
    let import_legacy = args.database.is_none()
        && database_url.is_none()
        && !bootstrap.exists()
        && legacy.is_file();
    let database = if let Some(path) = args.database {
        database::Database::Sqlite { path }
    } else if let Some(url) = database_url {
        database::Database::from_url(&url)?
    } else if bootstrap.exists() {
        serde_json::from_slice(&std::fs::read(&bootstrap)?)?
    } else if import_legacy {
        database::Database::Sqlite {
            path: std::fs::canonicalize(legacy)?,
        }
    } else {
        database::Database::Sqlite {
            path: directory.join("proxy.sqlite3"),
        }
    };
    let app = Arc::new(application::Application::open(database.open()?, args.port)?);
    if import_legacy {
        database.save(&bootstrap)?;
    }
    let app = Arc::new(management::Control {
        application: app,
        auth: auth::Auth::from_env()?,
        allowed_hosts: args.admin_host,
        bootstrap: Some(bootstrap),
        configured: std::sync::atomic::AtomicBool::new(true),
        traces: Some(traces::Traces::open(&directory.join("traces.sqlite3"))?),
    });
    let gateway = tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await?;
    let admin = tokio::net::TcpListener::bind((args.host.as_str(), args.admin_port)).await?;
    tracing::info!(anthropic = %gateway.local_addr()?, rest = %admin.local_addr()?, "Proxy iniciado; configura los proveedores desde el CLI");
    let (stop, _) = tokio::sync::watch::channel(false);
    let mut first = stop.subscribe();
    let mut second = stop.subscribe();
    let api = axum::serve(gateway, management::gateway_with_control(app.clone()))
        .with_graceful_shutdown(async move {
            let _ = first.changed().await;
        });
    let rest = axum::serve(admin, management::router_with_control(app.clone()))
        .with_graceful_shutdown(async move {
            let _ = second.changed().await;
        });
    let signal = async move {
        shutdown().await;
        let _ = stop.send(true);
    };
    let (a, b, _) = tokio::join!(api, rest, signal);
    if let Some(traces) = app.traces.clone() {
        tokio::task::spawn_blocking(move || traces.flush()).await??;
    }
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
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {}, _ = claude_proxy_service::stop_requested() => {} }
}
