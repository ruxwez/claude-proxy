use anyhow::{Context, Result, bail};
use clap::Parser;
use provider::Protocol;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

mod provider;
mod server;
mod stream;
#[cfg(test)]
mod tests;
mod translate;

struct Config {
    host: String,
    port: u16,
    timeout: Duration,
    providers: Vec<ProviderConfig>,
    client_version: String,
    project_id: String,
    protocols: HashMap<String, Protocol>,
    log_filter: String,
}

struct ProviderConfig {
    name: String,
    base: String,
    key: String,
    free_only: bool,
    models: Vec<String>,
}

// Read .env without mutating process-wide environment in an async runtime.
// Explicit environment variables take precedence over the file.
impl Config {
    fn load(path: &Path) -> Result<Self> {
        let mut values = HashMap::new();
        match dotenvy::from_path_iter(path) {
            Ok(iter) => {
                for item in iter {
                    let (key, value) = item?;
                    values.insert(key, value);
                }
            }
            Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("No se pudo leer .env"),
        }
        values.extend(std::env::vars());
        Self::from_values(&values)
    }

    fn from_values(values: &HashMap<String, String>) -> Result<Self> {
        let get =
            |key: &str, default: &str| values.get(key).cloned().unwrap_or_else(|| default.into());
        let enabled = |key: &str| -> Result<bool> {
            match get(key, "false").to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Ok(true),
                "false" | "0" | "no" | "off" | "" => Ok(false),
                _ => bail!("{key} debe ser booleano"),
            }
        };
        let mut providers = Vec::new();
        for (name, flag, url_key, base, free_only) in [
            (
                "opencode-zen",
                "OPENCODE_ZEN_ENABLED",
                "OPENCODE_ZEN_BASE_URL",
                "https://opencode.ai/zen/v1",
                true,
            ),
            (
                "opencode-go",
                "OPENCODE_GO_ENABLED",
                "OPENCODE_GO_BASE_URL",
                "https://opencode.ai/zen/go/v1",
                false,
            ),
            (
                "openai",
                "OPENAI_ENABLED",
                "OPENAI_BASE_URL",
                "https://api.openai.com/v1",
                false,
            ),
        ] {
            if enabled(flag)? {
                let base = get(url_key, base).trim_end_matches('/').to_string();
                let url = reqwest::Url::parse(&base).context("URL de proveedor inválida")?;
                if !["http", "https"].contains(&url.scheme()) {
                    bail!("URL de proveedor debe ser HTTP(S)");
                }
                providers.push(ProviderConfig {
                    name: name.into(),
                    base,
                    key: get(
                        if name == "openai" {
                            "OPENAI_API_KEY"
                        } else {
                            "OPENCODE_API_KEY"
                        },
                        "",
                    ),
                    free_only,
                    models: if name == "openai" {
                        get("OPENAI_MODELS", "")
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect()
                    } else {
                        Vec::new()
                    },
                });
            }
        }
        let protocols = serde_json::from_str(&get("MODEL_PROTOCOL_OVERRIDES", "{}"))
            .context("MODEL_PROTOCOL_OVERRIDES debe mapear IDs a chat, messages o responses")?;
        let seconds: u64 = get("REQUEST_TIMEOUT_SECONDS", "120")
            .parse()
            .context("Timeout inválido")?;
        let port = get("PROXY_PORT", "10800")
            .parse()
            .context("PROXY_PORT inválido")?;
        if seconds == 0 || port == 0 {
            bail!("Puerto y timeout deben ser mayores que cero");
        }
        Ok(Self {
            host: get("PROXY_HOST", "0.0.0.0"),
            port,
            timeout: Duration::from_secs(seconds),
            providers,
            client_version: get("OPENCODE_CLIENT_VERSION", "2.0.26"),
            project_id: get("OPENCODE_PROJECT_ID", ""),
            protocols,
            log_filter: get("RUST_LOG", "info"),
        })
    }
}

#[derive(Parser)]
#[command(version, about = "Proxy Anthropic para OpenCode y proveedores OpenAI")]
struct Args {
    #[arg(long, default_value = ".env")]
    env: PathBuf,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut config = Config::load(&args.env)?;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&config.log_filter)?)
        .init();
    if let Some(host) = args.host {
        config.host = host;
    }
    if let Some(port) = args.port {
        anyhow::ensure!(port > 0, "Puerto inválido");
        config.port = port;
    }
    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port)).await?;
    tracing::info!(address = %listener.local_addr()?, "claude-proxy Rust iniciado");
    axum::serve(listener, server::router(config)?)
        .with_graceful_shutdown(shutdown())
        .await?;
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
