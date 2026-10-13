use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub use claude_proxy_engine::Protocol;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpencodeGo,
    Openai,
    Chatgpt,
    Google,
    OpenaiCompatible,
    Zed,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ZedCredentials {
    pub user_id: u32,
    pub access_token: String,
    pub organization_id: String,
    pub system_id: String,
    pub client_version: String,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Credential {
    #[default]
    ApiKey,
    Subscription,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub client_id: String,
    pub subject: String,
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub credential: Credential,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zed_provider: Option<claude_proxy_engine::zed::Provider>,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default)]
    pub models_dev_provider: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub subscription: Option<Subscription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zed: Option<ZedCredentials>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub models: Vec<Model>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub providers: Vec<Provider>,
    pub default_model: Option<String>,
    pub context_tokens: Option<u64>,
    pub compact_tokens: Option<u64>,
    pub public_url: Option<String>,
    pub timeout_seconds: u64,
    pub host_id: String,
    pub traces: claude_proxy_engine::diagnostics::TracePolicy,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            traces: Default::default(),
            providers: vec![],
            default_model: None,
            context_tokens: Some(990_000),
            compact_tokens: Some(900_000),
            public_url: None,
            timeout_seconds: 120,
            host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        self.traces.validate(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        )?;
        ensure!(
            (1..=3600).contains(&self.timeout_seconds),
            "Timeout entre 1 y 3600 segundos"
        );
        ensure!(
            self.context_tokens != Some(0) && self.compact_tokens != Some(0),
            "Los límites deben ser positivos"
        );
        if let Some(compact) = self.compact_tokens {
            ensure!(
                self.context_tokens.is_some_and(|n| compact < n),
                "La compactación debe ser menor que el contexto"
            );
        }
        let mut providers = HashSet::new();
        let mut active_models = HashSet::new();
        for p in &self.providers {
            ensure!(
                !p.id.is_empty()
                    && p.id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
                "ID de proveedor: letras, números, guiones o guiones bajos"
            );
            ensure!(providers.insert(&p.id), "ID de proveedor duplicado");
            ensure!(!p.name.trim().is_empty(), "Nombre de proveedor requerido");
            let mut ids = HashSet::new();
            ensure!(
                p.kind == ProviderKind::Zed || p.zed.is_none(),
                "Credenciales Zed en un proveedor distinto"
            );
            if p.kind == ProviderKind::Zed {
                ensure!(
                    p.api_key.as_ref().is_none_or(|k| k.is_empty()),
                    "Zed utiliza autenticación de cuenta, no API key"
                );
                if let Some(auth) = &p.zed {
                    ensure!(
                        auth.user_id > 0
                            && !auth.access_token.trim().is_empty()
                            && !auth.organization_id.trim().is_empty()
                            && !auth.client_version.trim().is_empty(),
                        "Credenciales Zed incompletas"
                    );
                }
            }
            for m in &p.models {
                if p.kind == ProviderKind::Zed {
                    let provider = m.zed_provider.ok_or_else(|| {
                        anyhow::anyhow!("Selecciona el proveedor original del modelo Zed")
                    })?;
                    ensure!(
                        m.protocol == provider.protocol(),
                        "Protocolo incompatible con el proveedor original de Zed"
                    );
                } else {
                    ensure!(
                        m.zed_provider.is_none(),
                        "Proveedor original Zed en un modelo ajeno"
                    );
                }
                ensure!(
                    !m.id.is_empty() && m.id.trim() == m.id && !m.id.chars().any(char::is_control),
                    "ID de modelo inválido"
                );
                ensure!(ids.insert(&m.id), "Modelo duplicado en {}", p.id);
                if m.credential == Credential::Subscription {
                    ensure!(
                        p.kind == ProviderKind::Chatgpt && m.protocol == Protocol::Responses,
                        "ChatGPT por suscripción utiliza Responses"
                    );
                }
                ensure!(
                    p.kind != ProviderKind::Chatgpt || m.credential == Credential::Subscription,
                    "Los modelos de ChatGPT utilizan la suscripción"
                );
                if p.enabled && m.enabled {
                    active_models.insert(format!("{}/{}", p.id, m.id));
                }
            }
            ensure!(
                p.kind != ProviderKind::Chatgpt
                    || p.api_key.as_ref().is_none_or(|key| key.is_empty()),
                "ChatGPT no utiliza API key"
            );
            ensure!(
                p.kind == ProviderKind::Chatgpt || p.subscription.is_none(),
                "La suscripción debe estar en un proveedor ChatGPT independiente"
            );
        }
        if let Some(id) = &self.default_model {
            ensure!(
                active_models.contains(id),
                "El modelo predeterminado debe estar habilitado"
            );
        }
        Ok(())
    }

    pub fn models(&self) -> Vec<serde_json::Value> {
        let mut models: Vec<_> = self.providers.iter().filter(|p| p.enabled).flat_map(|p| {
            p.models.iter().filter(|m| m.enabled).map(|m| serde_json::json!({"id":format!("{}/{}",p.id,m.id), "display_name":if m.display_name.is_empty() { &m.id } else { &m.display_name }}))
        }).collect();
        models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        models
    }

    // Upgrade the previous combined OpenAI configuration without losing credentials.
    pub fn separate_chatgpt(&mut self) -> bool {
        let mut changed = false;
        let mut ids: HashSet<String> = self.providers.iter().map(|p| p.id.clone()).collect();
        let mut added = Vec::new();
        for p in &mut self.providers {
            if p.kind != ProviderKind::Openai
                || (p.subscription.is_none()
                    && !p
                        .models
                        .iter()
                        .any(|m| m.credential == Credential::Subscription))
            {
                continue;
            }
            changed = true;
            if p.api_key.as_ref().is_none_or(|key| key.is_empty())
                && p.models
                    .iter()
                    .all(|m| m.credential == Credential::Subscription)
            {
                p.kind = ProviderKind::Chatgpt;
                p.name = "ChatGPT".into();
                p.api_key = None;
                continue;
            }
            let mut id = format!("{}-chatgpt", p.id);
            let mut suffix = 2;
            while ids.contains(&id) {
                id = format!("{}-chatgpt-{suffix}", p.id);
                suffix += 1;
            }
            ids.insert(id.clone());
            let mut chatgpt = p.clone();
            chatgpt.id = id;
            chatgpt.name = format!("{} · ChatGPT", p.name);
            chatgpt.kind = ProviderKind::Chatgpt;
            chatgpt.api_key = None;
            chatgpt.base_url = "https://api.openai.com/v1".into();
            chatgpt
                .models
                .retain(|m| m.credential == Credential::Subscription);
            if let Some(default) = &self.default_model
                && chatgpt
                    .models
                    .iter()
                    .any(|m| *default == format!("{}/{}", p.id, m.id))
            {
                self.default_model = Some(format!(
                    "{}/{}",
                    chatgpt.id,
                    default.split_once('/').unwrap().1
                ));
            }
            p.subscription = None;
            p.models.retain(|m| m.credential == Credential::ApiKey);
            added.push(chatgpt);
        }
        self.providers.extend(added);
        changed
    }
}

// The single persistence port; HTTP and SQLite remain outside the domain.
pub trait SettingsStore: Send + Sync {
    fn load(&self) -> Result<Settings>;
    fn save(&self, settings: &Settings) -> Result<()>;
}

// Ephemeral authorization state, never persisted or returned with settings.
pub struct AuthorizationAttempt {
    pub provider: String,
    pub verifier: String,
    pub nonce: String,
    pub redirect: String,
    pub client_id: Option<String>,
    pub subject: Option<String>,
    pub created: std::time::Instant,
}
