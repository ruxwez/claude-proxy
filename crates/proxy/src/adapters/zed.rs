//! Zed HTTP and account adapter. Protocol conversion lives in the engine.
use crate::domain::{ProviderKind, ZedCredentials};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method, Response, StatusCode};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
pub const VERSION: &str = "1.23.3";

pub struct Config {
    pub credentials: Option<ZedCredentials>,
    pub models: HashMap<String, claude_proxy_engine::zed::Provider>,
}
pub struct Connection {
    base: String,
    http: Client,
    credentials: ZedCredentials,
    token: Mutex<Option<String>>,
}
impl Connection {
    pub fn new(base: &str, credentials: ZedCredentials, http: Client) -> Self {
        Self {
            base: base.trim_end_matches('/').into(),
            credentials,
            http,
            token: Mutex::new(None),
        }
    }
    async fn token(&self, rejected: Option<&str>) -> Result<String> {
        let mut token = self.token.lock().await;
        if let Some(value) = token.as_ref()
            && rejected != Some(value.as_str())
        {
            return Ok(value.clone());
        }
        *token = None;
        let response = self
            .http
            .post(format!("{}/client/llm_tokens", self.base))
            .header(
                "authorization",
                format!(
                    "{} {}",
                    self.credentials.user_id, self.credentials.access_token
                ),
            )
            .header("x-zed-system-id", &self.credentials.system_id)
            .json(&json!({"organization_id":self.credentials.organization_id}))
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .context("No se pudo obtener el token LLM de Zed")?;
        let value = read_json(response).await?;
        let value = value["token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("Zed no devolvió un token LLM")?
            .to_string();
        *token = Some(value.clone());
        Ok(value)
    }
    pub async fn send(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Response> {
        let mut token = self.token(None).await?;
        for attempt in 0..2 {
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .bearer_auth(&token)
                .header("x-zed-version", &self.credentials.client_version)
                .header("x-zed-client-supports-x-ai", "true")
                .header("x-zed-client-supports-status-messages", "true")
                .header(
                    "x-zed-client-supports-stream-ended-request-completion-status",
                    "true",
                );
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.context("Zed no alcanzable")?;
            let expired = response.status() == StatusCode::UNAUTHORIZED
                || response.headers().contains_key("x-zed-expired-token")
                || response.headers().contains_key("x-zed-outdated-token");
            if attempt == 0 && expired {
                token = self.token(Some(&token)).await?;
            } else {
                return Ok(response);
            }
        }
        unreachable!()
    }
    pub async fn models(&self) -> Result<Vec<Value>> {
        let value = read_json(self.send(Method::GET, "/models", None).await?).await?;
        let models = value["models"]
            .as_array()
            .context("Catálogo Zed sin models")?;
        let mut out = Vec::new();
        for model in models.iter().filter(|m| m["is_disabled"] != true) {
            let id = model["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("Modelo Zed sin ID")?;
            let provider: claude_proxy_engine::zed::Provider =
                serde_json::from_value(model["provider"].clone())
                    .context("Proveedor de modelo Zed desconocido")?;
            out.push(json!({"id":id,"display_name":model["display_name"].as_str().unwrap_or(id),"zed_provider":provider,"suggested_protocol":provider.protocol(),"metadata":model}));
        }
        Ok(out)
    }
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .read_timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
pub async fn read_json(mut response: Response) -> Result<Value> {
    // Never include a response body from an authentication endpoint in logs/errors.
    ensure!(
        response.status().is_success(),
        "Zed devolvió HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "Respuesta Zed demasiado grande"
        );
        bytes.extend(chunk);
    }
    serde_json::from_slice(&bytes).context("JSON de Zed inválido")
}

#[derive(serde::Deserialize)]
pub struct AccountInput {
    pub user_id: u32,
    pub access_token: String,
    #[serde(default)]
    pub organization_id: String,
    #[serde(default)]
    pub client_version: String,
}
pub async fn connect(
    app: &crate::application::Application,
    id: &str,
    input: AccountInput,
) -> Result<()> {
    connect_at(app, id, input, None).await
}
async fn connect_at(
    app: &crate::application::Application,
    id: &str,
    input: AccountInput,
    expected_base: Option<&str>,
) -> Result<()> {
    let before = app.settings();
    let provider = before
        .providers
        .iter()
        .find(|p| p.id == id && p.kind == ProviderKind::Zed)
        .context("Proveedor Zed no encontrado")?;
    ensure!(
        expected_base.is_none_or(|base| base == provider.base_url),
        "El proveedor cambió durante el acceso Zed"
    );
    ensure!(
        input.user_id > 0 && !input.access_token.trim().is_empty(),
        "Indica el ID de usuario y el token de cuenta Zed"
    );
    let http = client()?;
    let me = read_json(
        http.get(format!(
            "{}/client/users/me",
            provider.base_url.trim_end_matches('/')
        ))
        .header(
            "authorization",
            format!("{} {}", input.user_id, input.access_token),
        )
        .timeout(Duration::from_secs(20))
        .send()
        .await?,
    )
    .await?;
    let organization = if input.organization_id.trim().is_empty() {
        me["default_organization_id"]
            .as_str()
            .or_else(|| {
                me["organizations"]
                    .as_array()?
                    .iter()
                    .find(|o| o["is_personal"] == true)?["id"]
                    .as_str()
            })
            .context("Elige la organización Zed")?
            .to_string()
    } else {
        ensure!(
            me["organizations"]
                .as_array()
                .is_some_and(|items| items.iter().any(|o| o["id"] == input.organization_id)),
            "La cuenta no pertenece a esa organización Zed"
        );
        input.organization_id
    };
    let credentials = ZedCredentials {
        user_id: input.user_id,
        access_token: input.access_token,
        organization_id: organization,
        system_id: before.host_id.trim_start_matches("urn:uuid:").to_owned(),
        client_version: if input.client_version.trim().is_empty() {
            VERSION.into()
        } else {
            input.client_version
        },
    };
    let connection = Connection::new(&provider.base_url, credentials.clone(), http);
    connection.token(None).await?;
    let _guard = app.mutation.lock().await;
    let mut next = app.settings();
    let current = next
        .providers
        .iter_mut()
        .find(|p| p.id == id && p.kind == ProviderKind::Zed && p.base_url == provider.base_url)
        .context("El proveedor cambió durante la conexión")?;
    current.zed = Some(credentials);
    app.save(next).await
}
pub async fn disconnect(app: &crate::application::Application, id: &str) -> Result<()> {
    let _guard = app.mutation.lock().await;
    let mut next = app.settings();
    next.providers
        .iter_mut()
        .find(|p| p.id == id && p.kind == ProviderKind::Zed)
        .context("Proveedor Zed no encontrado")?
        .zed = None;
    if let Some(entries) = PENDING.lock().await.get_mut(&next.host_id) {
        entries.retain(|p| p.provider != id);
    }
    app.save(next).await
}

struct Pending {
    provider: String,
    base: String,
    key: rsa::RsaPrivateKey,
    expires: Instant,
}
static PENDING: LazyLock<Mutex<HashMap<String, Vec<Pending>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub async fn start(
    app: &crate::application::Application,
    id: &str,
    admin_port: u16,
) -> Result<String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE};
    use rsa::pkcs1::EncodeRsaPublicKey;
    let settings = app.settings();
    let provider = settings
        .providers
        .iter()
        .find(|p| p.id == id && p.kind == ProviderKind::Zed)
        .context("Proveedor Zed no encontrado")?;
    ensure!(
        provider.base_url.trim_end_matches('/') == "https://cloud.zed.dev",
        "El acceso por navegador requiere el endpoint oficial Zed"
    );
    let key =
        tokio::task::spawn_blocking(|| rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048))
            .await??;
    let public = URL_SAFE.encode(rsa::RsaPublicKey::from(&key).to_pkcs1_der()?.as_bytes());
    let mut url = reqwest::Url::parse("https://zed.dev/native_app_signin")?;
    url.query_pairs_mut()
        .append_pair("native_app_port", &admin_port.to_string())
        .append_pair("native_app_public_key", &public)
        .append_pair(
            "system_id",
            settings.host_id.trim_start_matches("urn:uuid:"),
        );
    let mut pending = PENDING.lock().await;
    // Entries are transient, bounded and never persisted with RSA keys.
    pending.retain(|_, entries| {
        entries.retain(|p| p.expires > Instant::now());
        !entries.is_empty()
    });
    ensure!(
        pending.values().map(Vec::len).sum::<usize>() < 8,
        "Demasiados accesos Zed pendientes"
    );
    let entries = pending.entry(settings.host_id).or_default();
    entries.retain(|p| p.provider != id);
    entries.push(Pending {
        provider: id.into(),
        base: provider.base_url.clone(),
        key,
        expires: Instant::now() + Duration::from_secs(300),
    });
    Ok(url.into())
}
pub async fn callback(
    app: &crate::application::Application,
    user_id: u32,
    encrypted: &str,
) -> Result<()> {
    use base64::{
        Engine,
        engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    };
    ensure!(encrypted.len() <= 2048, "Callback Zed inválido");
    let bytes = URL_SAFE
        .decode(encrypted)
        .or_else(|_| URL_SAFE_NO_PAD.decode(encrypted))
        .context("Callback Zed inválido")?;
    let (provider, base, token) = {
        let mut all = PENDING.lock().await;
        let entries = all
            .get_mut(&app.settings().host_id)
            .context("Acceso Zed no solicitado o caducado")?;
        entries.retain(|p| p.expires > Instant::now());
        let found = entries
            .iter()
            .enumerate()
            .find_map(|(i, p)| {
                p.key
                    .decrypt(rsa::Oaep::new::<sha2::Sha256>(), &bytes)
                    .ok()
                    .map(|token| (i, token))
            })
            .context("Callback Zed no corresponde a un acceso pendiente")?;
        let pending = entries.remove(found.0);
        (pending.provider, pending.base, String::from_utf8(found.1)?)
    };
    ensure!(
        app.settings()
            .providers
            .iter()
            .any(|p| p.id == provider && p.base_url == base),
        "El proveedor cambió durante el acceso Zed"
    );
    connect_at(
        app,
        &provider,
        AccountInput {
            user_id,
            access_token: token,
            organization_id: String::new(),
            client_version: String::new(),
        },
        Some(&base),
    )
    .await
}

pub async fn catalog(provider: &crate::domain::Provider) -> Result<Value> {
    let credentials = provider
        .zed
        .clone()
        .context("Conecta tu cuenta Zed primero")?;
    let data = Connection::new(&provider.base_url, credentials, client()?)
        .models()
        .await?;
    Ok(json!({"data":data,"metadata_source":"zed","warning":null}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::Application,
        domain::{Settings, SettingsStore},
        sqlite::SqliteStore,
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE};
    use rsa::pkcs1::DecodeRsaPublicKey;
    use std::sync::Arc;

    #[tokio::test]
    async fn browser_key_uses_native_zed_format_and_callback_is_single_use() {
        let (base, _, task) = crate::zed_tests::upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(&dir.path().join("settings.db")).unwrap());
        let settings: Settings = serde_json::from_value(json!({"providers":[{"id":"zed","name":"Zed","kind":"zed","base_url":"https://cloud.zed.dev"}]})).unwrap();
        store.save(&settings).unwrap();
        let app = Application::open(store, 10800).unwrap();
        let url = reqwest::Url::parse(&start(&app, "zed", 10801).await.unwrap()).unwrap();
        assert_eq!(url.host_str(), Some("zed.dev"));
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["native_app_port"], "10801");
        let public = rsa::RsaPublicKey::from_pkcs1_der(
            &URL_SAFE.decode(&query["native_app_public_key"]).unwrap(),
        )
        .unwrap();
        let encrypted = URL_SAFE.encode(
            public
                .encrypt(
                    &mut rsa::rand_core::OsRng,
                    rsa::Oaep::new::<sha2::Sha256>(),
                    b"account-secret",
                )
                .unwrap(),
        );
        assert!(callback(&app, 42, "AAAA").await.is_err());
        // Bind the same pending key to a disposable HTTP fixture: no real Zed account is used.
        let mut settings = app.settings();
        settings.providers[0].base_url = base.clone();
        app.save(settings.clone()).await.unwrap();
        PENDING.lock().await.get_mut(&settings.host_id).unwrap()[0].base = base;
        callback(&app, 42, &encrypted).await.unwrap();
        assert_eq!(
            app.settings().providers[0]
                .zed
                .as_ref()
                .unwrap()
                .access_token,
            "account-secret"
        );
        assert!(callback(&app, 42, &encrypted).await.is_err());
        assert!(
            !app.settings().providers[0]
                .zed
                .as_ref()
                .unwrap()
                .system_id
                .starts_with("urn:uuid:")
        );
        task.abort();
    }
}
