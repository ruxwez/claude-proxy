use crate::ProviderConfig;
use anyhow::{Context, Result};
use reqwest::{Client, RequestBuilder, header::HeaderMap};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex as AsyncMutex;

pub use crate::domain::Protocol;

pub use claude_proxy_engine::protocol_for_model as protocol;

pub struct Provider {
    zed: Option<crate::zed::Connection>,
    pub config: ProviderConfig,
    client: Client,
    user_agent: String,
    project: String,
    session_prefix: String,
    cache: AsyncMutex<Option<(Instant, Vec<Value>)>>,
}

impl Provider {
    pub fn new(
        config: ProviderConfig,
        client: Client,
        user_agent: String,
        project: String,
    ) -> Self {
        let zed = config
            .zed
            .as_ref()
            .and_then(|z| z.credentials.clone())
            .map(|auth| crate::zed::Connection::new(&config.base, auth, client.clone()));
        Self {
            zed,
            client,
            user_agent,
            project,
            config,
            session_prefix: format!("ses_{}", time_hex(true)),
            cache: AsyncMutex::new(None),
        }
    }

    pub async fn send(
        &self,
        protocol: Protocol,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<reqwest::Response> {
        if self.config.zed.is_some() {
            return self
                .zed
                .as_ref()
                .context("Conecta tu cuenta Zed desde el CLI")?
                .send(reqwest::Method::POST, "/completions", Some(body))
                .await;
        }
        let mut request = self.post(protocol, headers);
        if protocol == Protocol::Responses
            && let Some(key) = body["prompt_cache_key"].as_str()
        {
            // ChatGPT derives cache affinity from this header as well as the
            // Responses body key (see openai/codex commit bc5957eac9).
            request = request.header("session-id", key);
        }
        request
            .json(body)
            .send()
            .await
            .context("Proveedor no alcanzable")
    }

    pub fn prompt_cache_key(&self, headers: &HeaderMap, req: &Value) -> String {
        let session = ["x-claude-code-session-id", "x-session-id", "session-id"]
            .iter()
            .find_map(|name| {
                headers
                    .get(*name)?
                    .to_str()
                    .ok()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            });
        let user = req["metadata"]["user_id"].as_str().unwrap_or_default();
        let metadata = serde_json::from_str::<Value>(user).ok();
        let session = session
            .or_else(|| {
                metadata
                    .as_ref()?
                    .get("session_id")?
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
            })
            .or_else(|| {
                user.rsplit_once("_session_")
                    .map(|(_, id)| id)
                    .filter(|id| !id.is_empty())
            });
        let identity = if let Some(session) = session {
            json!({"session":session})
        } else {
            // Stateless fallback: appending turns must not change the route.
            // Use the initial user turn, not the entire growing conversation.
            json!({"user":user,"first_message":req["messages"].as_array().and_then(|all| all.first())})
        };
        let material = json!([self.config.name, req["model"], identity]);
        // Never expose a client's identifier or prompt in provider headers.
        format!("{:x}", Sha256::digest(material.to_string().as_bytes()))
    }

    pub async fn models(&self, refresh: bool) -> Result<Vec<Value>> {
        // Hold a per-provider lock to avoid a stampede on cache expiry.
        let mut cache = self.cache.lock().await;
        if !refresh
            && let Some((at, data)) = &*cache
            && at.elapsed() < Duration::from_secs(300)
        {
            return Ok(data.clone());
        }
        let raw = if !self.config.models.is_empty() {
            self.config
                .models
                .iter()
                .map(|id| json!({"id":id}))
                .collect()
        } else {
            let mut req = self
                .client
                .get(format!("{}/models", self.config.base))
                .header("accept", "application/json");
            if !self.config.key.is_empty() {
                req = req.bearer_auth(&self.config.key);
            }
            if self.config.opencode {
                req = req
                    .header("user-agent", &self.user_agent)
                    .header("x-opencode-client", "cli");
            }
            let resp = req
                .send()
                .await
                .context("No se pudo consultar el catálogo")?
                .error_for_status()?;
            let value: Value = resp.json().await?;
            value["data"]
                .as_array()
                .context("Catálogo sin data")?
                .clone()
        };
        let mut models = Vec::new();
        for item in raw {
            let id = item["id"].as_str().unwrap_or_default().trim();
            if id.is_empty() || (self.config.free_only && !id.to_lowercase().contains("free")) {
                continue;
            }
            let created =
                chrono::DateTime::from_timestamp(item["created"].as_i64().unwrap_or(0), 0)
                    .unwrap_or_default()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            models.push(json!({"type":"model","id":format!("{}/{id}",self.config.name),"display_name":item["display_name"].as_str().unwrap_or(id),"created_at":created}));
        }
        models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        models.dedup_by(|a, b| a["id"] == b["id"]);
        *cache = Some((Instant::now(), models.clone()));
        Ok(models)
    }

    pub fn post(&self, protocol: Protocol, headers: &HeaderMap) -> RequestBuilder {
        let path = match protocol {
            Protocol::Chat => "/chat/completions",
            Protocol::Messages => "/messages",
            Protocol::Responses => "/responses",
        };
        self.post_path(path, protocol, headers)
    }

    pub fn post_path(&self, path: &str, protocol: Protocol, headers: &HeaderMap) -> RequestBuilder {
        let mut req = self.client.post(format!("{}{path}", self.config.base));
        // Incoming client credentials are never forwarded to a provider.
        if !self.config.key.is_empty() {
            req = if protocol == Protocol::Messages {
                req.header("x-api-key", &self.config.key)
            } else {
                req.bearer_auth(&self.config.key)
            };
        }
        if self.config.opencode {
            let session = self.session(headers);
            req = req
                .header("user-agent", &self.user_agent)
                .header("x-opencode-project", &self.project)
                .header("x-opencode-client", "cli")
                .header("x-opencode-session", session)
                .header("x-opencode-request", identifier("msg", false));
            if let Some(v) = headers.get("x-claude-code-session-id") {
                req = req.header("x-claude-code-session-id", v);
            }
        }
        if protocol == Protocol::Messages {
            req = req.header(
                "anthropic-version",
                headers
                    .get("anthropic-version")
                    .and_then(|s| s.to_str().ok())
                    .unwrap_or("2023-06-01"),
            );
            if let Some(value) = headers.get("anthropic-beta") {
                req = req.header("anthropic-beta", value);
            }
        }
        req
    }

    fn session(&self, headers: &HeaderMap) -> String {
        for name in [
            "x-opencode-session",
            "x-claude-code-session-id",
            "x-session-id",
        ] {
            if let Some(id) = headers
                .get(name)
                .and_then(|s| s.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if valid_identifier(id, "ses") {
                    return id.into();
                }
                let hash = Sha256::digest(id.as_bytes());
                return format!("{}{}", self.session_prefix, base62(&hash[..14]));
            }
        }
        identifier("ses", true)
    }
}

static LAST_ID: AtomicU64 = AtomicU64::new(0);
fn time_hex(descending: bool) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let clock = millis << 12;
    let mut previous = LAST_ID.load(Ordering::Relaxed);
    let current = loop {
        let current = clock.max(previous + 1);
        match LAST_ID.compare_exchange_weak(previous, current, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break current,
            Err(actual) => previous = actual,
        }
    };
    let value = current & 0xffffffffffff;
    format!(
        "{:012x}",
        if descending {
            !value & 0xffffffffffff
        } else {
            value
        }
    )
}
fn base62(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    bytes
        .iter()
        .map(|b| CHARS[*b as usize % 62] as char)
        .collect()
}
pub fn identifier(prefix: &str, descending: bool) -> String {
    format!(
        "{prefix}_{}{}",
        time_hex(descending),
        base62(&uuid::Uuid::new_v4().as_bytes()[..14])
    )
}
pub fn valid_identifier(id: &str, prefix: &str) -> bool {
    let prefix = format!("{prefix}_");
    id.len() == prefix.len() + 26
        && id.starts_with(&prefix)
        && id.as_bytes()[prefix.len()..prefix.len() + 12]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        && id.as_bytes()[prefix.len() + 12..]
            .iter()
            .all(u8::is_ascii_alphanumeric)
}

pub fn selected_protocol(
    provider: &str,
    model: &str,
    overrides: &HashMap<String, Protocol>,
) -> Protocol {
    let key = format!("{provider}/{model}");
    if let Some(p) = overrides.get(&key).or_else(|| overrides.get(model)) {
        return *p;
    }
    if provider.starts_with("opencode-") {
        protocol(model)
    } else {
        Protocol::Chat
    }
}
