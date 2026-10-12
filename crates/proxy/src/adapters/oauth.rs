use crate::{
    application::Application,
    domain::{ProviderKind, Subscription},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::domain::AuthorizationAttempt as Pending;
fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

pub async fn start(app: &Application, id: &str, ui_port: u16) -> Result<String> {
    let settings = app.settings();
    let provider = settings
        .providers
        .iter()
        .find(|p| p.id == id && p.kind == ProviderKind::Chatgpt)
        .context("Proveedor ChatGPT no encontrado")?;
    let state = random();
    let pending = Pending {
        provider: id.into(),
        verifier: random(),
        nonce: random(),
        redirect: format!("http://127.0.0.1:{ui_port}/auth/callback"),
        client_id: provider.subscription.as_ref().map(|s| s.client_id.clone()),
        subject: provider.subscription.as_ref().map(|s| s.subject.clone()),
        created: Instant::now(),
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes()));
    let mut url = reqwest::Url::parse("https://auth.openai.com/api/accounts/authorize")?;
    url.query_pairs_mut().extend_pairs([
        (
            "client_id",
            pending
                .client_id
                .as_deref()
                .unwrap_or("dynamic_agent_client"),
        ),
        ("ext_agent_host_id", settings.host_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", &pending.redirect),
        (
            "scope",
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
        ),
        ("resource", "https://api.openai.com/v1"),
        ("state", &state),
        ("nonce", &pending.nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
    ]);
    if pending.client_id.is_none() {
        url.query_pairs_mut()
            .append_pair("agent_name_hint", "claude-proxy");
    }
    // No token hints in browser URLs: returning users choose their account again.
    let mut attempts = app.pending.lock().await;
    attempts.retain(|_, p| p.created.elapsed() < Duration::from_secs(600));
    ensure!(
        attempts.len() < 16,
        "Demasiados intentos de inicio de sesión"
    );
    attempts.insert(state, pending);
    Ok(url.to_string())
}

#[derive(Deserialize)]
pub struct Callback {
    state: String,
    code: Option<String>,
    client_id: Option<String>,
    error: Option<String>,
}
#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    nonce: String,
    #[serde(default)]
    email: String,
}

pub async fn complete(app: &Application, callback: Callback) -> Result<()> {
    let pending = app
        .pending
        .lock()
        .await
        .remove(&callback.state)
        .context("Estado OAuth no válido o ya utilizado")?;
    ensure!(
        pending.created.elapsed() < Duration::from_secs(600),
        "El inicio de sesión ha caducado"
    );
    ensure!(
        callback.error.is_none(),
        "Inicio de sesión cancelado o no autorizado"
    );
    let issued = match (&pending.client_id, callback.client_id) {
        (Some(saved), Some(received)) => {
            ensure!(saved == &received, "El cliente OAuth no coincide");
            received
        }
        (Some(saved), None) => saved.clone(),
        (None, Some(received)) if received != "dynamic_agent_client" && !received.is_empty() => {
            received
        }
        _ => anyhow::bail!("Registro incompleto: falta el client_id emitido"),
    };
    let code = callback.code.context("Falta el código OAuth")?;
    let http = client()?;
    let response = http
        .post("https://auth.openai.com/api/accounts/oauth/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &issued),
            ("code", &code),
            ("code_verifier", &pending.verifier),
            ("redirect_uri", &pending.redirect),
            ("resource", "https://api.openai.com/v1"),
        ])
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "OpenAI rechazó el intercambio OAuth ({})",
        response.status()
    );
    let tokens: Tokens = response.json().await?;
    ensure!(
        tokens.scope.as_deref().is_some_and(|s| s
            .split_whitespace()
            .any(|s| s == "chatgpt.tokens.use.direct")),
        "No se ha autorizado el uso del plan ChatGPT"
    );
    let id_token = tokens.id_token.context("Falta el ID token")?;
    let header = decode_header(&id_token)?;
    let discovery: serde_json::Value = http
        .get("https://auth.openai.com/.well-known/openid-configuration")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let jwks_uri = discovery["jwks_uri"].as_str().context("Falta JWKS")?;
    ensure!(
        jwks_uri.starts_with("https://auth.openai.com/"),
        "JWKS fuera del emisor esperado"
    );
    let jwks: JwkSet = http
        .get(jwks_uri)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let key = jwks
        .find(header.kid.as_deref().context("ID token sin kid")?)
        .context("Clave de firma desconocida")?;
    let mut validation = Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_audience(&[&issued]);
    validation.set_issuer(&["https://auth.openai.com"]);
    let claims = decode::<Claims>(&id_token, &DecodingKey::from_jwk(key)?, &validation)?.claims;
    ensure!(claims.nonce == pending.nonce, "Nonce OAuth no válido");
    ensure!(
        pending
            .subject
            .as_ref()
            .is_none_or(|sub| sub == &claims.sub),
        "La cuenta no coincide; añade otro proveedor para otra cuenta"
    );
    let _guard = app.mutation.lock().await;
    let mut settings = app.settings();
    let p = settings
        .providers
        .iter_mut()
        .find(|p| p.id == pending.provider && p.kind == ProviderKind::Chatgpt)
        .context("El proveedor ha sido eliminado")?;
    p.subscription = Some(Subscription {
        client_id: issued,
        subject: claims.sub,
        email: claims.email,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        id_token,
        expires_at: now() + tokens.expires_in,
    });
    app.save(settings).await
}

pub async fn refresh(app: &Application, model: &str) -> Result<()> {
    let Some((id, model_id)) = model.split_once('/') else {
        return Ok(());
    };
    refresh_selected(app, id, Some(model_id)).await
}

pub async fn refresh_subscription(app: &Application, provider_id: &str) -> Result<()> {
    refresh_selected(app, provider_id, None).await
}

async fn refresh_selected(app: &Application, id: &str, model_id: Option<&str>) -> Result<()> {
    // API-key requests and valid sessions never wait for another account's renewal.
    let current = app.settings();
    let selected = current.providers.iter().find(|p| {
        (p.enabled || model_id.is_none())
            && p.id == id
            && model_id.is_none_or(|model_id| {
                p.models.iter().any(|m| {
                    m.enabled
                        && m.id == model_id
                        && m.credential == crate::domain::Credential::Subscription
                })
            })
    });
    let Some(selected) = selected else {
        return Ok(());
    };
    let subscription = selected
        .subscription
        .as_ref()
        .context("Conecta tu cuenta ChatGPT desde su página")?;
    if subscription.expires_at > now() + 60 {
        return Ok(());
    }
    // Serialize rotating refresh tokens and configuration updates.
    let _guard = app.mutation.lock().await;
    let mut settings = app.settings();
    let Some(subscription) = settings
        .providers
        .iter()
        .find(|p| {
            (p.enabled || model_id.is_none())
                && p.id == id
                && model_id.is_none_or(|model_id| {
                    p.models.iter().any(|m| {
                        m.enabled
                            && m.id == model_id
                            && m.credential == crate::domain::Credential::Subscription
                    })
                })
        })
        .map(|p| p.subscription.as_ref())
    else {
        return Ok(());
    };
    let subscription = subscription.context("Conecta tu cuenta ChatGPT desde su página")?;
    if subscription.expires_at > now() + 60 {
        return Ok(());
    }
    let response = client()?
        .post("https://auth.openai.com/api/accounts/oauth/token")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &subscription.client_id),
            ("refresh_token", &subscription.refresh_token),
            ("resource", "https://api.openai.com/v1"),
        ])
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "No se pudo renovar la suscripción ({}); vuelve a conectar desde el CLI",
        response.status()
    );
    let tokens: Tokens = response.json().await?;
    if let Some(scope) = &tokens.scope {
        ensure!(
            scope
                .split_whitespace()
                .any(|s| s == "chatgpt.tokens.use.direct"),
            "El plan ChatGPT ya no está autorizado"
        );
    }
    let subscription = settings
        .providers
        .iter_mut()
        .find(|p| p.id == id)
        .unwrap()
        .subscription
        .as_mut()
        .unwrap();
    subscription.access_token = tokens.access_token;
    subscription.refresh_token = tokens.refresh_token;
    subscription.expires_at = now() + tokens.expires_in;
    if let Some(id_token) = tokens.id_token {
        subscription.id_token = id_token;
    }
    app.save(settings).await
}
