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
    pub key: String,
    http: reqwest::Client,
}
impl Client {
    #[cfg(test)]
    pub fn new(endpoint: &str) -> Result<Self> {
        Self::with_key(endpoint, "")
    }
    pub fn with_key(endpoint: &str, key: &str) -> Result<Self> {
        let (endpoint, _) = crate::urls(endpoint)?;
        let mut headers = reqwest::header::HeaderMap::new();
        if !key.is_empty() {
            let mut value =
                reqwest::header::HeaderValue::from_str(key).context("Clave inválida")?;
            value.set_sensitive(true);
            headers.insert("x-api-key", value);
        }
        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none());
        let url = reqwest::Url::parse(&endpoint)?;
        if url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }) {
            // Discovery must remain on loopback even when HTTP_PROXY is set.
            builder = builder.no_proxy();
        }
        Ok(Self {
            endpoint,
            key: key.into(),
            http: builder.build()?,
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
        ensure!(
            status != reqwest::StatusCode::UNAUTHORIZED,
            "El servidor requiere una clave válida; configúrala en Conexión o con claude-proxy config key"
        );
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
    pub async fn probe(&self) -> Result<Value> {
        let response = self
            .http
            .get(format!("{}/api/status", self.endpoint))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .context("Servidor no disponible")?;
        ensure!(
            response.status() != reqwest::StatusCode::UNAUTHORIZED,
            "El servidor requiere una clave válida"
        );
        ensure!(
            response.status().is_success(),
            "El servidor rechazó la comprobación"
        );
        let value: Value = response
            .json()
            .await
            .context("Respuesta de estado inválida")?;
        ensure!(
            value["service"] == "claude-proxy",
            "El endpoint no es Claude Proxy"
        );
        Ok(value)
    }
}
#[derive(Default, Serialize, Deserialize)]
pub struct Connection {
    pub endpoint: String,
    #[serde(default)]
    pub credential_store: Option<String>,
}
pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_PROXY_CONFIG_DIR") {
        return Ok(PathBuf::from(path).join("config.json"));
    }
    Ok(directories::ProjectDirs::from("", "", "claude-proxy")
        .context("Directorio de configuración no disponible")?
        .config_dir()
        .join("config.json"))
}
fn entry(endpoint: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new("claude-proxy", endpoint)?)
}
pub fn connection() -> Result<Option<Connection>> {
    let path = config_path()?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(
        serde_json::from_slice(&bytes).context("Configuración del CLI inválida")?,
    ))
}
pub fn saved() -> Result<Option<Client>> {
    let path = config_path()?;
    let Some(saved) = connection()? else {
        return Ok(None);
    };
    let key = match saved.credential_store.as_deref() {
        Some("keyring") => entry(&saved.endpoint)?
            .get_password()
            .context("No se pudo leer la clave del almacén del SO; usa config key")?,
        Some("file") => String::from_utf8(std::fs::read(path.with_file_name("auth-key"))?)?,
        _ => String::new(),
    };
    Ok(Some(Client::with_key(&saved.endpoint, &key)?))
}
pub fn private_write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().context("Falta directorio")?;
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("icacls.exe")
            .arg(directory)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{}:(OI)(CI)F", std::env::var("USERNAME")?))
            .args(["/grant:r", "*S-1-5-18:(OI)(CI)F", "/Q"])
            .stdout(std::process::Stdio::null())
            .status()?;
        ensure!(
            status.success(),
            "No se pudo proteger el directorio de configuración"
        );
    }
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub fn remember(client: &Client) -> Result<()> {
    let path = config_path()?;
    let secret = path.with_file_name("auth-key");
    let store = if client.key.is_empty() {
        None
    } else {
        if entry(&client.endpoint)
            .and_then(|e| e.set_password(&client.key).map_err(Into::into))
            .is_ok()
        {
            Some("keyring")
        } else {
            private_write(&secret, client.key.as_bytes())?;
            Some("file")
        }
    };
    let previous = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Connection>(&b).ok());
    private_write(
        &path,
        &serde_json::to_vec_pretty(&Connection {
            endpoint: client.endpoint.clone(),
            credential_store: store.map(Into::into),
        })?,
    )?;
    if store != Some("file") && secret.exists() {
        std::fs::remove_file(secret)?;
    }
    if let Some(previous) = previous
        && previous.credential_store.as_deref() == Some("keyring")
        && (previous.endpoint != client.endpoint || store != Some("keyring"))
    {
        let _ = entry(&previous.endpoint)?.delete_credential();
    }
    Ok(())
}
fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    ensure!(io::stdin().read_line(&mut line)? > 0, "Entrada cerrada");
    Ok(line.trim().into())
}
pub fn prompt_key() -> Result<String> {
    Ok(rpassword::prompt_password(
        "Clave opcional (vacío: sin clave): ",
    )?)
}
pub async fn configure(endpoint: &str, key: &str) -> Result<Client> {
    let client = Client::with_key(endpoint, key)?;
    client.probe().await?;
    remember(&client)?;
    Ok(client)
}
pub async fn connect(endpoint: &str, key: &str, interactive: bool) -> Result<Client> {
    if !endpoint.is_empty() {
        let stored = saved()
            .ok()
            .flatten()
            .filter(|s| crate::urls(endpoint).is_ok_and(|(base, _)| base == s.endpoint));
        let key = if key.is_empty() {
            stored.as_ref().map(|s| s.key.as_str()).unwrap_or("")
        } else {
            key
        };
        return configure(endpoint, key).await;
    }
    if let Some(mut client) = saved().unwrap_or_else(|error| {
        eprintln!("Configuración: {error}");
        None
    }) {
        if !key.is_empty() {
            client = Client::with_key(&client.endpoint, key)?;
        }
        match client.probe().await {
            Ok(_) => {
                if !key.is_empty() {
                    remember(&client)?;
                }
                return Ok(client);
            }
            Err(error) => eprintln!("Conexión guardada: {error}"),
        }
    }
    // Deliberately only literal loopback addresses, no DNS/LAN enumeration.
    for endpoint in ["http://127.0.0.1:10801", "http://[::1]:10801"] {
        let client = Client::with_key(endpoint, key)?;
        if client.probe().await.is_ok() {
            if !interactive
                || !io::stdin().is_terminal()
                || prompt("Servidor local encontrado. ¿Utilizarlo? [S/n]: ")?.to_lowercase() != "n"
            {
                remember(&client)?;
                return Ok(client);
            }
            break;
        }
    }
    ensure!(
        interactive && io::stdin().is_terminal(),
        "No hay servidor funcional. Usa claude-proxy service install o claude-proxy config connect ENDPOINT"
    );
    loop {
        println!("1. Instalar servidor local (SQLite)\n2. Conectar a servidor existente");
        match prompt("Modalidad [1/2]: ")?.as_str() {
            "1" => {
                let binary = crate::installation::prepare_server().await?;
                crate::installation::install(binary)?;
                let client = Client::with_key("http://127.0.0.1:10801", "")?;
                wait_ready(&client).await?;
                remember(&client)?;
                return Ok(client);
            }
            "2" => {
                let endpoint = prompt("Endpoint del servidor: ")?;
                let key = prompt_key()?;
                match configure(&endpoint, &key).await {
                    Ok(client) => return Ok(client),
                    Err(error) => eprintln!("No se pudo conectar: {error}"),
                }
            }
            _ => eprintln!("Elige 1 o 2."),
        }
    }
}
pub async fn wait_ready(client: &Client) -> Result<()> {
    for _ in 0..30 {
        if client.probe().await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("El servicio no respondió; consulta claude-proxy service status")
}
