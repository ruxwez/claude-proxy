use crate::{
    Config, ProviderConfig,
    domain::ProviderKind,
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
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

struct Proxy {
    catalog: Option<Vec<Value>>,
    providers: Vec<Provider>,
    protocols: HashMap<String, Protocol>,
    subscriptions: HashSet<String>,
    traces: Option<Arc<crate::traces::Traces>>,
    trace_policy: claude_proxy_engine::diagnostics::TracePolicy,
}

#[cfg(test)]
pub fn router(config: Config) -> Result<Router> {
    router_with_subscriptions(config, HashSet::new())
}

#[cfg(test)]
pub(crate) fn router_with_subscriptions(
    config: Config,
    subscriptions: HashSet<String>,
) -> Result<Router> {
    router_with_diagnostics(config, subscriptions, None, Default::default())
}
fn router_with_diagnostics(
    config: Config,
    subscriptions: HashSet<String>,
    traces: Option<Arc<crate::traces::Traces>>,
    trace_policy: claude_proxy_engine::diagnostics::TracePolicy,
) -> Result<Router> {
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
        tracing::info!("Sin modelos activos; configura proveedores desde el CLI");
    }
    Ok(Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/models/{*id}", get(model))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(Arc::new(Proxy {
            catalog: config.catalog,
            providers,
            protocols: config.protocols,
            subscriptions,
            traces,
            trace_policy,
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
                let kind = match status.as_u16() {
                    400 | 422 => "invalid_request_error",
                    401 => "authentication_error",
                    403 => "permission_error",
                    404 => "not_found_error",
                    413 => "request_too_large",
                    429 => "rate_limit_error",
                    529 | 503 => "overloaded_error",
                    _ => "api_error",
                };
                (status, kind, m)
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
            .filter(|_| proxy.catalog.is_none())
            .map(|p| async move { (p, p.models(query.refresh).await) }),
    )
    .await;
    let mut data = proxy.catalog.clone().unwrap_or_default();
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
    if let Some(catalog) = &proxy.catalog {
        return catalog
            .iter()
            .find(|m| m["id"] == id)
            .cloned()
            .map(Json)
            .ok_or_else(|| ApiError::NotFound("Modelo no configurado".into()));
    }
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
    if proxy
        .catalog
        .as_ref()
        .is_some_and(|models| !models.iter().any(|m| m["id"] == id))
    {
        return Err(ApiError::NotFound(
            "Modelo no configurado o deshabilitado".into(),
        ));
    }
    let (name, model) = id
        .trim()
        .split_once('/')
        .filter(|(name, model)| !name.is_empty() && !model.trim().is_empty())
        .ok_or_else(|| ApiError::NotFound("Usa conector/modelo".into()))?;
    let provider = proxy
        .providers
        .iter()
        .find(|p| {
            p.config.name == name
                && (proxy.catalog.is_none() || p.config.models.iter().any(|id| id == model))
        })
        .ok_or_else(|| ApiError::NotFound(format!("Conector no activo: {name}")))?;
    Ok((provider, model.trim()))
}

async fn messages(
    State(proxy): State<Arc<Proxy>>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(req) = payload?;
    let capture = proxy
        .traces
        .as_ref()
        .and_then(|t| t.start(&proxy.trace_policy));
    if let Some(trace) = &capture {
        trace.stage("client_request", &req);
        trace.finish("invalid_request");
    }
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
    if let Some(trace) = &capture {
        trace.route(id, protocol);
    }
    let started = Instant::now();
    tracing::info!(model = id, ?protocol, "Enviando petición al proveedor");
    let zed_provider = p
        .config
        .zed
        .as_ref()
        .map(|z| {
            z.models
                .get(target)
                .copied()
                .context("Modelo Zed sin proveedor original")
        })
        .transpose()?;
    let mut body = if let Some(provider) = zed_provider {
        claude_proxy_engine::zed::request(&req, target, provider)
    } else {
        translate::request(&req, target, protocol)
    }
    .map_err(|e| ApiError::Invalid(e.to_string()))?;
    let mut warnings = if let Some(provider) = zed_provider {
        claude_proxy_engine::zed::warnings(&req, target, provider)
    } else {
        translate::warnings(&req, target, protocol)
    };
    if proxy.subscriptions.contains(&p.config.name) {
        translate::adapt_subscription_request(&mut body, &mut warnings);
    }
    if (protocol != Protocol::Messages || zed_provider.is_some())
        && headers.contains_key("anthropic-beta")
    {
        warnings.push("anthropic_beta_not_transferable");
    }
    if !warnings.is_empty() {
        tracing::warn!(model = id, ?warnings, "Adaptaciones de protocolo");
    }
    if let Some(trace) = &capture {
        trace.stage("upstream_request", &body);
        trace.warnings(&warnings);
        trace.finish("transport_error");
    }
    let upstream = p.send(protocol, &headers, &body).await?;
    tracing::info!(model = id, status = %upstream.status(), elapsed_ms = started.elapsed().as_millis(), "Cabeceras del proveedor recibidas");
    if let Some(trace) = &capture {
        trace.transport(upstream.status().as_u16(), started.elapsed().as_millis());
    }
    if !upstream.status().is_success() {
        let status = upstream.status();
        let transport = forward_headers(upstream.headers());
        let retry_after = upstream.headers().get(RETRY_AFTER).cloned();
        let bytes = read_limited(upstream, 1024 * 1024).await?;
        let raw = serde_json::from_slice::<Value>(&bytes).ok();
        if let Some(trace) = &capture {
            trace.stage("upstream_response", raw.as_ref().unwrap_or(&Value::Null));
            trace.finish("upstream_error");
        }
        if protocol == Protocol::Messages
            && let Some(value) = raw.as_ref().filter(|v| v["type"] == "error")
        {
            return Ok((status, transport, Json(value)).into_response());
        }
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(2000).collect());
        let status = if status.is_client_error() || status.is_server_error() {
            status
        } else {
            StatusCode::BAD_GATEWAY
        };
        let mut response = ApiError::Provider(status, message, retry_after).into_response();
        response.headers_mut().extend(transport);
        return Ok(response);
    }
    let upstream_headers = upstream.headers().clone();
    let mut response = if let Some(provider) = zed_provider {
        stream::zed_response(upstream, provider, id.to_string(), &req, capture.clone()).await?
    } else if req["stream"].as_bool().unwrap_or(false) {
        stream::response_traced(upstream, protocol, id.to_string(), &req, capture.clone())
    } else {
        let raw: Value = serde_json::from_slice(&read_limited(upstream, 32 * 1024 * 1024).await?)
            .context("JSON del proveedor inválido")?;
        if let Some(trace) = &capture {
            trace.stage("upstream_response", &raw);
            trace.finish("conversion_error");
        }
        let mut converted = translate::response(&raw, id, protocol)?;
        if protocol != Protocol::Messages {
            translate::restore_tool_names(&mut converted, &req);
            translate::apply_stops(&mut converted, &req)?;
        }
        if let Some(trace) = &capture {
            trace.comparison(&claude_proxy_engine::diagnostics::compare_response(
                &raw, &converted, &req, protocol,
            ));
            trace.stage("client_response", &converted);
            trace.finish("completed");
        }
        Json(converted).into_response()
    };
    if let Some(trace) = &capture {
        response.headers_mut().insert(
            "x-claude-proxy-trace-id",
            HeaderValue::from_str(&trace.id()).unwrap(),
        );
    }
    response
        .headers_mut()
        .extend(forward_headers(&upstream_headers));
    if !warnings.is_empty() {
        response.headers_mut().insert(
            "x-claude-proxy-warnings",
            HeaderValue::from_str(&warnings.join(",")).unwrap(),
        );
    }
    Ok(response)
}

fn forward_headers(headers: &HeaderMap) -> HeaderMap {
    headers
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "request-id" | "x-request-id" | "retry-after" | "openai-processing-ms"
            ) || name.as_str().starts_with("anthropic-ratelimit-")
                || name.as_str().starts_with("x-ratelimit-")
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
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
    State(proxy): State<Arc<Proxy>>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(req) = payload?;
    if let Some(id) = req["model"].as_str() {
        let (provider, model) = find_provider(&proxy, id)?;
        let protocol = selected_protocol(&provider.config.name, model, &proxy.protocols);
        if protocol != Protocol::Chat && provider.config.zed.is_none() {
            let body = translate::count_request(
                &req,
                model,
                protocol,
                proxy.subscriptions.contains(&provider.config.name),
            )
            .map_err(|e| ApiError::Invalid(e.to_string()))?;
            let path = if protocol == Protocol::Messages {
                "/messages/count_tokens"
            } else {
                "/responses/input_tokens"
            };
            let upstream = provider
                .post_path(path, protocol, &headers)
                .json(&body)
                .send()
                .await
                .context("Conteo del proveedor no alcanzable")?;
            let status = upstream.status();
            let transport = forward_headers(upstream.headers());
            if status.is_success() {
                let raw: Value =
                    serde_json::from_slice(&read_limited(upstream, 1024 * 1024).await?)
                        .context("Conteo del proveedor inválido")?;
                let count = raw["input_tokens"]
                    .as_u64()
                    .context("Conteo del proveedor sin input_tokens")?;
                let mut response = (transport, Json(json!({"input_tokens":count}))).into_response();
                response.headers_mut().insert(
                    "x-claude-proxy-token-count",
                    HeaderValue::from_static("provider"),
                );
                return Ok(response);
            }
            // Only an absent endpoint permits estimation. Authentication, rate
            // limits and invalid input must remain explicit provider failures.
            if !matches!(status.as_u16(), 404 | 405 | 501) {
                let retry = upstream.headers().get(RETRY_AFTER).cloned();
                let raw = read_limited(upstream, 1024 * 1024).await?;
                let value = serde_json::from_slice::<Value>(&raw).ok();
                if protocol == Protocol::Messages
                    && let Some(value) = value.as_ref().filter(|v| v["type"] == "error")
                {
                    return Ok((status, transport, Json(value)).into_response());
                }
                let message = value
                    .as_ref()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("Conteo rechazado por el proveedor: {status}"));
                let mut response = ApiError::Provider(status, message, retry).into_response();
                response.headers_mut().extend(transport);
                return Ok(response);
            }
            // The endpoint can be unavailable while still reporting a real quota.
            let mut response = estimated_count(&req);
            response.headers_mut().extend(transport);
            return Ok(response);
        }
    }
    Ok(estimated_count(&req))
}

fn estimated_count(req: &Value) -> Response {
    (
        [("x-claude-proxy-token-count", "estimated")],
        Json(json!({"input_tokens":translate::estimated_tokens(req)})),
    )
        .into_response()
}
pub fn configured_router_with_traces(
    settings: &crate::domain::Settings,
    traces: Option<Arc<crate::traces::Traces>>,
) -> Result<Router> {
    settings.validate()?;
    let mut providers = Vec::new();
    let mut protocols = HashMap::new();
    let mut subscriptions = HashSet::new();
    for p in &settings.providers {
        if !p.enabled {
            continue;
        }
        let models: Vec<_> = p
            .models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| {
                protocols.insert(format!("{}/{}", p.id, m.id), m.protocol);
                m.id.clone()
            })
            .collect();
        if models.is_empty() {
            continue;
        }
        let (base, key) = if p.kind == ProviderKind::Chatgpt {
            subscriptions.insert(p.id.clone());
            (
                "https://api.openai.com/v1".to_string(),
                p.subscription
                    .as_ref()
                    .map(|s| s.access_token.clone())
                    .unwrap_or_default(),
            )
        } else {
            (
                p.base_url.trim_end_matches('/').to_string(),
                p.api_key.clone().unwrap_or_default(),
            )
        };
        providers.push(ProviderConfig {
            zed: (p.kind == ProviderKind::Zed).then(|| crate::zed::Config {
                credentials: p.zed.clone(),
                models: p
                    .models
                    .iter()
                    .filter_map(|m| m.zed_provider.map(|kind| (m.id.clone(), kind)))
                    .collect(),
            }),
            opencode: p.kind == ProviderKind::OpencodeGo,
            name: p.id.clone(),
            base,
            key,
            models,
            free_only: false,
        });
        if p.kind == ProviderKind::Google {
            ensure!(
                p.models
                    .iter()
                    .all(|m| m.protocol == crate::domain::Protocol::Chat),
                "Google usa el endpoint compatible Chat Completions"
            );
        }
    }
    let catalog = settings
        .models()
        .into_iter()
        .map(|mut m| {
            m["type"] = "model".into();
            m["created_at"] = "1970-01-01T00:00:00Z".into();
            m
        })
        .collect();
    router_with_diagnostics(
        Config {
            catalog: Some(catalog),
            #[cfg(test)]
            port: 10800,
            timeout: Duration::from_secs(settings.timeout_seconds),
            providers,
            protocols,
            client_version: "2.0.26".into(),
            project_id: settings.host_id.clone(),
        },
        subscriptions,
        traces,
        settings.traces.clone(),
    )
}
