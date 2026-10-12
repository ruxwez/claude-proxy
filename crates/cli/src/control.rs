use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};

#[derive(Clone)]
pub struct Client {
    pub endpoint: String,
    http: reqwest::Client,
}
impl Client {
    pub fn new(endpoint: &str) -> Result<Self> {
        let (endpoint, _) = crate::urls(endpoint)?;
        Ok(Self {
            endpoint,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.endpoint))
            .header("Content-Type", "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .context("No se pudo conectar con el servidor")?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .context("La API no devolvió JSON válido")?;
        ensure!(
            status.is_success(),
            "{}",
            value["error"]
                .as_str()
                .unwrap_or("La API rechazó la operación")
        );
        Ok(value)
    }
    pub async fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None).await
    }
    pub async fn put_settings(&self, settings: &Value) -> Result<()> {
        self.request(reqwest::Method::PUT, "/api/settings", Some(settings))
            .await?;
        Ok(())
    }
    async fn available(&self) -> bool {
        match self
            .http
            .get(format!("{}/api/status", self.endpoint))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response
                .json::<Value>()
                .await
                .is_ok_and(|v| v["service"] == "claude-proxy"),
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Connection {
    endpoint: String,
}
pub fn config_path() -> Result<PathBuf> {
    Ok(directories::ProjectDirs::from("", "", "claude-proxy")
        .context("Directorio de configuración no disponible")?
        .config_dir()
        .join("config.json"))
}
fn remember(endpoint: &str) -> Result<()> {
    let path = config_path()?;
    let directory = path.parent().unwrap();
    std::fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(
        &mut file,
        &Connection {
            endpoint: endpoint.into(),
        },
    )?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub async fn connect(endpoint: &str) -> Result<Client> {
    let explicit = endpoint != "http://localhost:10801";
    let local = Client::new(if explicit {
        endpoint
    } else {
        "http://127.0.0.1:10801"
    })?;
    if local.available().await {
        if explicit {
            remember(&local.endpoint)?;
        }
        return Ok(local);
    }
    if !explicit
        && let Ok(bytes) = std::fs::read(config_path()?)
        && let Ok(saved) = serde_json::from_slice::<Connection>(&bytes)
    {
        let client = Client::new(&saved.endpoint)?;
        if client.available().await {
            return Ok(client);
        }
    }
    ensure!(
        io::stdin().is_terminal(),
        "Proxy no encontrado. Inicia claude-proxy-server o indica --endpoint http://HOST:10801"
    );
    loop {
        print!("Proxy no encontrado. Endpoint REST (Ctrl+C para salir): ");
        io::stdout().flush()?;
        let mut line = String::new();
        ensure!(io::stdin().read_line(&mut line)? > 0, "Entrada cerrada");
        match Client::new(line.trim()) {
            Ok(client) if client.available().await => {
                remember(&client.endpoint)?;
                return Ok(client);
            }
            _ => eprintln!("No se encontró una API Claude Proxy en ese endpoint."),
        }
    }
}
