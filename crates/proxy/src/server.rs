use crate::{
    Config,
    provider::{Protocol, Provider, selected_protocol},
    stream, translate,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

struct Proxy {
    providers: Vec<Provider>,
    protocols: HashMap<String, Protocol>,
}

pub fn router(config: Config) -> Result<Router> {
    let client = reqwest::Client::builder()
        .read_timeout(config.timeout)
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let user_agent = format!(
        "opencode/{} ai-sdk/provider-utils/4.0.23 runtime/bun/1.4.0",
        config.client_version
    );
    let project = if config.project_id.is_empty() {
        uuid::Uuid::new_v4().simple().to_string()
    } else {
        config.project_id
    };
    let providers: Vec<_> = config
        .providers
        .into_iter()
        .map(|p| Provider::new(p, client.clone(), user_agent.clone(), project.clone()))
        .collect();
    if providers.is_empty() {
        tracing::warn!("Ningún conector activo; revisa .env");
    }
    Ok(Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/models/{*id}", get(model))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(Arc::new(Proxy {
            providers,
            protocols: config.protocols,
        })))
}

// One boundary converts errors to Anthropic responses; handlers can use `?`.
enum ApiError {
    Invalid(String),
    NotFound(String),
    Upstream(anyhow::Error),
    Provider(StatusCode, String, Option<HeaderValue>),
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::Upstream(error)
    }
}
impl From<JsonRejection> for ApiError {
    fn from(error: JsonRejection) -> Self {
        Self::Invalid(error.body_text())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        let (status, kind, message) = match self {
            Self::Invalid(m) => (StatusCode::BAD_REQUEST, "invalid_request_error", m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, "not_found_error", m),
            Self::Upstream(e) => {
                let timeout = e.chain().any(|cause| {
                    cause
                        .downcast_ref::<reqwest::Error>()
                        .is_some_and(reqwest::Error::is_timeout)
                });
                tracing::warn!(timeout, error = %e, cause = %e.root_cause(), "Fallo al consultar el proveedor");
                (
                    if timeout {
                        StatusCode::GATEWAY_TIMEOUT
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    "api_error",
                    format!("{e:#}"),
                )
            }
            Self::Provider(status, m, retry_after) => {
                tracing::warn!(%status, retry_after = ?retry_after, "El proveedor rechazó la petición");
                if let Some(value) = retry_after {
                    headers.insert(RETRY_AFTER, value);
                }
                (status, "api_error", m)
            }
        };
        (
            status,
            headers,
            Json(json!({"type":"error","error":{"type":kind,"message":message}})),
        )
            .into_response()
    }
}

async fn health(State(proxy): State<Arc<Proxy>>) -> Json<Value> {
    Json(
        json!({"status":"ok","implementation":"rust","connectors":proxy.providers.iter().map(|p|&p.config.name).collect::<Vec<_>>()}),
    )
}

#[derive(Deserialize)]
struct ModelQuery {
    limit: Option<usize>,
    after_id: Option<String>,
    before_id: Option<String>,
    #[serde(default)]
    refresh: bool,
}

async fn models(
    State(proxy): State<Arc<Proxy>>,
    Query(query): Query<ModelQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(usize::MAX);
    if limit == 0 {
        return Err(ApiError::Invalid("limit debe ser mayor que cero".into()));
    }
    let results = join_all(
        proxy
            .providers
            .iter()
            .map(|p| async move { (p, p.models(query.refresh).await) }),
    )
    .await;
    let mut data = Vec::new();
    let mut errors = Vec::new();
    for (p, result) in results {
        match result {
            Ok(items) => data.extend(items),
            Err(e) => {
                tracing::warn!(connector=%p.config.name,error=%e,"Catálogo no disponible");
                errors.push(json!({"connector":p.config.name,"message":e.to_string()}));
            }
        }
    }
    if data.is_empty() && !errors.is_empty() {
        return Err(anyhow::anyhow!("No se pudo consultar ningún catálogo de modelos").into());
    }
    data.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    if let Some(after) = query.after_id {
        data.retain(|m| m["id"].as_str().unwrap_or_default() > after.as_str());
    }
    if let Some(before) = query.before_id {
        data.retain(|m| m["id"].as_str().unwrap_or_default() < before.as_str());
    }
    let has_more = data.len() > limit;
    data.truncate(limit);
    let mut out = json!({"first_id":data.first().map(|m|&m["id"]),"last_id":data.last().map(|m|&m["id"]),"data":data,"has_more":has_more});
    if !errors.is_empty() {
        out["errors"] = json!(errors);
    }
    Ok(Json(out))
}

async fn model(
    State(proxy): State<Arc<Proxy>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (p, _) = find_provider(&proxy, &id)?;
    let model = p
        .models(false)
        .await?
        .into_iter()
        .find(|m| m["id"] == id)
        .ok_or_else(|| ApiError::NotFound("Modelo no encontrado".into()))?;
    Ok(Json(model))
}

fn find_provider<'a>(proxy: &'a Proxy, id: &'a str) -> Result<(&'a Provider, &'a str), ApiError> {
    let (name, model) = id
        .trim()
        .split_once('/')
        .filter(|(name, model)| !name.is_empty() && !model.trim().is_empty())
        .ok_or_else(|| ApiError::NotFound("Usa conector/modelo".into()))?;
    let provider = proxy
        .providers
        .iter()
        .find(|p| p.config.name == name)
        .ok_or_else(|| ApiError::NotFound(format!("Conector no activo: {name}")))?;
    Ok((provider, model.trim()))
}

async fn messages(
    State(proxy): State<Arc<Proxy>>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(req) = payload?;
    let id = req["model"].as_str().unwrap_or_default().trim();
    if id.is_empty()
        || req["max_tokens"].as_u64().unwrap_or(0) == 0
        || req["messages"].as_array().is_none_or(|m| m.is_empty())
    {
        return Err(ApiError::Invalid(
            "model, max_tokens > 0 y messages no vacío son requeridos".into(),
        ));
    }
    let (p, target) = find_provider(&proxy, id)?;
    let protocol = selected_protocol(&p.config.name, target, &proxy.protocols);
    let started = Instant::now();
    tracing::info!(model = id, ?protocol, "Enviando petición al proveedor");
    let body =
        translate::request(&req, target, protocol).map_err(|e| ApiError::Invalid(e.to_string()))?;
    let upstream = p
        .post(protocol, &headers)
        .json(&body)
        .send()
        .await
        .context("Proveedor no alcanzable")?;
    tracing::info!(model = id, status = %upstream.status(), elapsed_ms = started.elapsed().as_millis(), "Cabeceras del proveedor recibidas");
    if !upstream.status().is_success() {
        let status = upstream.status();
        let retry_after = upstream.headers().get(RETRY_AFTER).cloned();
        let bytes = read_limited(upstream, 1024 * 1024).await?;
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(2000).collect());
        let status = if status.is_client_error() || status.is_server_error() {
            status
        } else {
            StatusCode::BAD_GATEWAY
        };
        return Err(ApiError::Provider(status, message, retry_after));
    }
    if req["stream"].as_bool().unwrap_or(false) {
        return Ok(stream::response(upstream, protocol, id.to_string()));
    }
    let raw: Value = serde_json::from_slice(&read_limited(upstream, 32 * 1024 * 1024).await?)
        .context("JSON del proveedor inválido")?;
    Ok(Json(translate::response(&raw, id, protocol)?).into_response())
}

async fn read_limited(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= max,
            "Respuesta del proveedor demasiado grande"
        );
        bytes.extend(chunk);
    }
    Ok(bytes)
}

async fn count_tokens(
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(req) = payload?;
    let size = text_size(&req["system"]) + text_size(&req["messages"]) + text_size(&req["tools"]);
    Ok(Json(json!({"input_tokens":(size/4).max(1)})))
}
fn text_size(v: &Value) -> usize {
    match v {
        Value::String(s) => s.chars().count(),
        Value::Array(a) => a.iter().map(text_size).sum(),
        Value::Object(o) => o.values().map(text_size).sum(),
        _ => 0,
    }
}
