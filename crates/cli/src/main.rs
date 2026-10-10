use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "Sincroniza los modelos de un proxy Anthropic con Claude Code"
)]
struct Args {
    /// Catálogo de capacidades de models.dev
    #[arg(long, default_value = "https://models.dev/catalog.json?type=all")]
    models_url: String,
    /// URL base del proxy, o URL completa de /v1/models
    #[arg(
        long,
        env = "CLAUDE_PROXY_URL",
        default_value = "http://localhost:10800"
    )]
    endpoint: String,
    /// Archivo settings.json; por defecto CLAUDE_CONFIG_DIR o ~/.claude/settings.json
    #[arg(long)]
    settings: Option<PathBuf>,
    /// Modelo predeterminado explícito (ID exacto del catálogo)
    #[arg(long)]
    default_model: Option<String>,
    /// Ver cambios sin escribir archivos
    #[arg(long)]
    dry_run: bool,
    /// Timeout HTTP en segundos
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
    /// Credencial del proxy, nunca la clave del proveedor
    #[arg(
        long,
        env = "CLAUDE_PROXY_API_KEY",
        default_value = "local-opencode-go"
    )]
    api_key: String,
}

#[derive(Deserialize)]
struct Model {
    id: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Deserialize)]
struct Page {
    data: Vec<Model>,
    #[serde(default)]
    has_more: bool,
    last_id: Option<String>,
    #[serde(default)]
    errors: Vec<Value>,
}

fn urls(endpoint: &str) -> Result<(String, reqwest::Url)> {
    let mut url = reqwest::Url::parse(endpoint).context("Endpoint inválido")?;
    ensure!(
        ["http", "https"].contains(&url.scheme()) && url.host_str().is_some(),
        "Endpoint debe ser HTTP(S)"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Usa --api-key para la credencial"
    );
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_string();
    let base_path = path
        .strip_suffix("/v1/models")
        .or_else(|| path.strip_suffix("/v1"))
        .unwrap_or(&path)
        .to_string();
    url.set_path(&base_path);
    let base = url.as_str().trim_end_matches('/').to_string();
    let models = reqwest::Url::parse(&format!("{base}/v1/models"))?;
    Ok((base, models))
}

async fn fetch_models(url: reqwest::Url, timeout: u64, key: &str) -> Result<Vec<Model>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut models = BTreeMap::new();
    let mut cursor = None::<String>;
    let mut cursors = HashSet::new();
    for _ in 0..1000 {
        let mut request = client
            .get(url.clone())
            .header("x-api-key", key)
            .bearer_auth(key)
            .query(&[("refresh", "true"), ("limit", "1000")]);
        if let Some(cursor) = &cursor {
            request = request.query(&[("after_id", cursor)]);
        }
        let mut response = request
            .send()
            .await
            .context("No se pudo conectar con el proxy")?
            .error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 16 * 1024 * 1024,
                "Catálogo demasiado grande"
            );
            bytes.extend(chunk);
        }
        let page: Page = serde_json::from_slice(&bytes).context("JSON de modelos inválido")?;
        ensure!(
            page.errors.is_empty(),
            "El proxy devolvió un catálogo parcial; no se modifica Claude Code"
        );
        ensure!(
            !page.has_more || !page.data.is_empty(),
            "Paginación inválida: página vacía con has_more"
        );
        for model in page.data {
            ensure!(
                !model.id.trim().is_empty()
                    && model.id.trim() == model.id
                    && !model.id.chars().any(char::is_control),
                "ID de modelo inválido"
            );
            models.insert(model.id.clone(), model);
        }
        if !page.has_more {
            ensure!(
                !models.is_empty(),
                "Catálogo vacío; no se modifica Claude Code"
            );
            return Ok(models.into_values().collect());
        }
        let next = page
            .last_id
            .filter(|s| !s.is_empty())
            .context("Paginación sin last_id")?;
        ensure!(
            cursors.insert(next.clone()),
            "El proxy repite el cursor; no se modifica Claude Code"
        );
        cursor = Some(next);
    }
    bail!("Demasiadas páginas en el catálogo")
}

async fn fetch_contexts(url: &str, timeout: u64) -> Result<Value> {
    // This request deliberately carries no proxy credentials.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client
        .get(url)
        .send()
        .await
        .context("No se pudo consultar models.dev")?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 32 * 1024 * 1024,
            "Catálogo de models.dev demasiado grande"
        );
        bytes.extend(chunk);
    }
    let catalog: Value = serde_json::from_slice(&bytes).context("JSON de models.dev inválido")?;
    ensure!(
        catalog["providers"]
            .as_object()
            .is_some_and(|p| !p.is_empty())
            && catalog["models"].is_object(),
        "models.dev debe contener los objetos providers y models"
    );
    Ok(catalog)
}

fn filter_models(models: Vec<Model>, catalog: &Value) -> (Vec<Model>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut excluded = Vec::new();
    for model in models {
        let entry = model
            .id
            .split_once('/')
            .map(|(provider, name)| &catalog["providers"][provider]["models"][name]);
        let entry = entry.unwrap_or(&Value::Null);
        let canonical = entry["canonical_model_id"]
            .as_str()
            .map(|id| &catalog["models"][id])
            .unwrap_or(&Value::Null);
        // Only an explicit canonical link can supply missing limits. A provider's
        // own value always wins, including smaller or invalid values.
        let limit = |name: &str| {
            entry["limit"]
                .get(name)
                .or_else(|| canonical["limit"].get(name))
        };
        let context = limit("context").and_then(Value::as_u64);
        let input = limit("input");
        match context {
            Some(n) if n >= 1_000_000 => match input.map(Value::as_u64) {
                Some(None) => excluded.push(format!(
                    "{}: límite de entrada inválido en models.dev",
                    model.id
                )),
                Some(Some(n)) if n < 990_000 => excluded.push(format!(
                    "{}: entrada máxima de {n} tokens, inferior a 990000",
                    model.id
                )),
                _ => accepted.push(model),
            },
            Some(n) => excluded.push(format!("{}: contexto de {n} tokens", model.id)),
            None => excluded.push(format!("{}: contexto desconocido en models.dev", model.id)),
        }
    }
    (accepted, excluded)
}

fn resolve_model<'a>(model: &'a str, config: &'a Value) -> &'a str {
    let key = match model {
        "haiku" => "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "sonnet" => "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "opus" => "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "fable" | "best" => "ANTHROPIC_DEFAULT_FABLE_MODEL",
        _ => return model,
    };
    config["env"][key].as_str().unwrap_or(model)
}

fn configured(
    mut config: Value,
    models: &[Model],
    base: &str,
    key: &str,
    explicit: Option<&str>,
) -> Result<Value> {
    ensure!(config.is_object(), "settings.json debe ser un objeto JSON");
    ensure!(
        config.get("env").is_none_or(Value::is_object),
        "env debe ser un objeto JSON"
    );
    let ids: HashSet<_> = models.iter().map(|m| m.id.as_str()).collect();
    ensure!(!ids.is_empty(), "Catálogo vacío");
    let current = config["model"]
        .as_str()
        .map(|s| resolve_model(s, &config))
        .filter(|s| ids.contains(s));
    let default = if let Some(explicit) = explicit {
        ensure!(ids.contains(explicit), "Modelo no disponible: {explicit}");
        explicit.to_string()
    } else {
        current
            .or_else(|| ids.get("opencode-go/deepseek-v4.1-flash").copied())
            .unwrap_or(&models[0].id)
            .to_string()
    };
    let old_haiku = config["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"]
        .as_str()
        .filter(|s| ids.contains(s))
        .map(String::from);
    let haiku = old_haiku.or_else(|| {
        models
            .iter()
            .filter(|m| m.id.contains("claude-haiku"))
            .max_by_key(|m| version_key(&m.id))
            .map(|m| m.id.clone())
    });
    config["model"] = json!(default);
    if config.get("env").is_none() {
        config["env"] = json!({});
    }
    config["env"]["ANTHROPIC_BASE_URL"] = json!(base);
    config["env"]["ANTHROPIC_API_KEY"] = json!(key);
    if config["env"].get("ANTHROPIC_AUTH_TOKEN").is_some() {
        config["env"]["ANTHROPIC_AUTH_TOKEN"] = json!(key);
    }
    config["env"]["CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS"] = json!("1");
    config["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"] = json!("990000");
    config["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"] = json!("900000");
    for flag in [
        "DISABLE_COMPACT",
        "DISABLE_AUTO_COMPACT",
        "CLAUDE_CODE_DISABLE_1M_CONTEXT",
        "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT",
    ] {
        config["env"].as_object_mut().unwrap().remove(flag);
    }
    for alias in [
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        let missing = config["env"][alias]
            .as_str()
            .is_some_and(|s| !ids.contains(resolve_model(s, &config)));
        if missing
            || (alias == "ANTHROPIC_DEFAULT_SONNET_MODEL" && config["env"].get(alias).is_none())
        {
            config["env"][alias] = json!(default);
            for suffix in ["_NAME", "_DESCRIPTION", "_SUPPORTED_CAPABILITIES"] {
                config["env"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&format!("{alias}{suffix}"));
            }
        }
    }
    config["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"] = json!(haiku.unwrap_or(default.clone()));
    config["modelPicker"] = json!({"replaceBuiltInOptions":true,"options":models.iter().map(|m|json!({"model":m.id,"label":if m.display_name.is_empty(){&m.id}else{&m.display_name},"description":format!("{} · {}",base,m.id)})).collect::<Vec<_>>()});
    Ok(config)
}

fn version_key(id: &str) -> Vec<u64> {
    id.split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse().ok())
        .collect()
}

struct Report {
    count: usize,
    excluded: Vec<String>,
    default: String,
    path: PathBuf,
    backup: Option<PathBuf>,
    changed: bool,
}

async fn run(args: &Args) -> Result<Report> {
    let (base, url) = urls(&args.endpoint)?;
    let models = fetch_models(url, args.timeout, &args.api_key).await?;
    let contexts = fetch_contexts(&args.models_url, args.timeout).await?;
    let (models, excluded) = filter_models(models, &contexts);
    ensure!(
        !models.is_empty(),
        "Ningún modelo tiene contexto verificado de al menos 1.000.000 tokens; no se modifica Claude Code.\n{}",
        excluded.join("\n")
    );
    let path = match &args.settings {
        Some(path) => path.clone(),
        None => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|home| home.join(".claude")))
            .context("No se pudo determinar el directorio de Claude; usa --settings")?
            .join("settings.json"),
    };
    // Read only after the network call; lock spans the read/modify/replace cycle.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let _lock = if args.dry_run {
        None
    } else {
        fs::create_dir_all(parent)?;
        let lock_path = parent.join(format!(
            "{}.update-models.lock",
            path.file_name()
                .context("Ruta settings inválida")?
                .to_string_lossy()
        ));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.try_lock()
            .context("Otro claude-update-models está actualizando este archivo")?;
        Some(lock)
    };
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(
            !metadata.file_type().is_symlink(),
            "settings.json es un enlace simbólico: indica su destino con --settings"
        );
    }
    let original = match fs::read(&path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let previous: Value = if let Some(raw) = &original {
        serde_json::from_slice(raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(raw))
            .context("settings.json inválido; no se ha modificado")?
    } else {
        json!({})
    };
    let next = configured(
        previous.clone(),
        &models,
        &base,
        &args.api_key,
        args.default_model.as_deref(),
    )?;
    let changed = previous != next;
    let mut report = Report {
        count: models.len(),
        excluded,
        default: next["model"].as_str().unwrap_or_default().into(),
        path: path.clone(),
        backup: None,
        changed,
    };
    if !changed || args.dry_run {
        return Ok(report);
    }
    let permissions = fs::metadata(&path).ok().map(|m| m.permissions());
    if let Some(original) = &original {
        let dir = parent.join("backups");
        fs::create_dir_all(&dir)?;
        let backup = dir.join(format!(
            "settings.before-model-sync.{}.json",
            uuid::Uuid::new_v4()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&backup)?;
        if let Some(permissions) = &permissions {
            file.set_permissions(permissions.clone())?;
        }
        file.write_all(original)?;
        file.sync_all()?;
        report.backup = Some(backup);
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(permissions) = permissions {
        temp.as_file().set_permissions(permissions)?;
    }
    serde_json::to_writer_pretty(&mut temp, &next)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    // Claude Code itself doesn't participate in our lock; detect intervening edits.
    let current = fs::read(&path).ok();
    ensure!(
        current == original,
        "settings.json cambió durante la actualización; vuelve a intentarlo"
    );
    temp.persist(&path)
        .map_err(|e| e.error)
        .context("No se pudo reemplazar settings.json")?;
    Ok(report)
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match run(&args).await {
        Ok(report) => {
            println!(
                "{}: {} modelos. Predeterminado: {}",
                if args.dry_run {
                    "Vista previa"
                } else if report.changed {
                    "Actualizado"
                } else {
                    "Sin cambios"
                },
                report.count,
                report.default
            );
            println!("Configuración: {}", report.path.display());
            println!("Contexto: 990.000 tokens; compactación automática: 900.000 tokens.");
            for reason in &report.excluded {
                println!("Excluido: {reason}");
            }
            if let Some(backup) = report.backup {
                println!("Copia de seguridad: {}", backup.display());
            }
            if args.dry_run {
                println!("No se ha escrito ningún archivo.");
            } else if report.changed {
                println!("Reinicia Claude Code y ejecuta /model.");
            }
        }
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests;
