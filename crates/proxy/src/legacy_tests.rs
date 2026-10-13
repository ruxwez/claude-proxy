use super::*;
use anyhow::{Context, bail};
use std::path::Path;
// Read .env without mutating process-wide environment in an async runtime.
// Explicit environment variables take precedence over the file.
impl Config {
    pub(crate) fn load(path: &Path) -> Result<Self> {
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

    pub(crate) fn from_values(values: &HashMap<String, String>) -> Result<Self> {
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
                    zed: None,
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
                    opencode: name.starts_with("opencode-"),
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
            catalog: None,
            port,
            timeout: Duration::from_secs(seconds),
            providers,
            client_version: get("OPENCODE_CLIENT_VERSION", "2.0.26"),
            project_id: get("OPENCODE_PROJECT_ID", ""),
            protocols,
        })
    }
}
