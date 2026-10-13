use crate::{application::Application, domain::Settings, oauth};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

// Composition belongs to the driving adapter, not the application or domain.
pub struct Control {
    pub application: Arc<Application>,
    pub allowed_hosts: Vec<String>,
    pub traces: Option<Arc<crate::traces::Traces>>,
    pub bootstrap: Option<std::path::PathBuf>,
    pub configured: std::sync::atomic::AtomicBool,
}
impl std::ops::Deref for Control {
    type Target = Arc<Application>;
    fn deref(&self) -> &Self::Target {
        &self.application
    }
}
#[cfg(test)]
fn control(application: Arc<Application>) -> Arc<Control> {
    Arc::new(Control {
        application,
        allowed_hosts: vec![],
        traces: None,
        bootstrap: None,
        configured: std::sync::atomic::AtomicBool::new(true),
    })
}
#[cfg(test)]
pub fn router(app: Arc<Application>) -> Router {
    router_with_control(control(app))
}
#[cfg(test)]
pub fn gateway(app: Arc<Application>) -> Router {
    gateway_with_control(control(app))
}

type Failure = (StatusCode, Json<Value>);
fn failure(error: impl std::fmt::Display) -> Failure {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error.to_string()})),
    )
}
pub fn router_with_control(app: Arc<Control>) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/setup", post(setup))
        .route("/api/traces", get(list_traces).delete(clear_traces))
        .route("/api/traces/{id}", get(trace_detail))
        .route("/api/settings", get(settings).put(save))
        .route("/api/cli-config", get(cli_config))
        .route("/api/providers/catalog", post(provider_catalog))
        .route("/api/metadata/providers", get(metadata_providers))
        .route(
            "/api/providers/{id}/oauth",
            post(start_oauth).delete(disconnect),
        )
        .route("/api/providers/{id}/api-key", delete(clear_key))
        .route("/auth/callback", get(callback))
        .route("/", get(zed_callback))
        .route("/api/providers/{id}/zed/auth", post(zed_start))
        .route(
            "/api/providers/{id}/zed/credentials",
            post(zed_connect).delete(zed_disconnect),
        )
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), protect))
        .with_state(app)
}

// Local admin has no login prompt. Reject foreign browser origins and DNS rebinding.
async fn protect(State(app): State<Arc<Control>>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|s| s.to_str().ok())
        .unwrap_or("");
    let parsed = reqwest::Url::parse(&format!("http://{host}"));
    let valid_host = parsed
        .as_ref()
        .ok()
        .and_then(|u| u.host_str())
        .is_some_and(|h| {
            h == "localhost"
                || app
                    .allowed_hosts
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(h))
                || h.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok()
        });
    let valid_origin = request.headers().get(header::ORIGIN).is_none_or(|origin| {
        origin
            .to_str()
            .ok()
            .is_some_and(|o| o == format!("http://{host}") || o == format!("https://{host}"))
    });
    let foreign = request.uri().path().starts_with("/api/")
        && request
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site");
    let mutation = matches!(
        *request.method(),
        axum::http::Method::POST | axum::http::Method::PUT | axum::http::Method::DELETE
    );
    let json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next() == Some("application/json"));
    if !valid_host || !valid_origin || foreign || (mutation && !json) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
}
async fn status(State(app): State<Arc<Control>>) -> Json<Value> {
    Json(
        json!({"service":"claude-proxy","version":env!("CARGO_PKG_VERSION"),"configured":app.configured.load(std::sync::atomic::Ordering::Acquire),"anthropic_port":app.port}),
    )
}
async fn setup(
    State(app): State<Arc<Control>>,
    Json(database): Json<crate::database::Database>,
) -> Result<Json<Value>, Failure> {
    let _guard = app.mutation.lock().await;
    if app.configured.load(std::sync::atomic::Ordering::Acquire) {
        return Err(failure("El servidor ya está configurado"));
    }
    let path = app
        .bootstrap
        .clone()
        .ok_or_else(|| failure("Servidor sin bootstrap"))?;
    let mut database = database;
    if let crate::database::Database::Sqlite { path: sqlite } = &mut database
        && sqlite.as_os_str().is_empty()
    {
        *sqlite = path.parent().unwrap().join("proxy.sqlite3");
    }
    let previous = app.settings();
    let (store, settings) = tokio::task::spawn_blocking(move || {
        let store = database.open()?;
        let mut settings = store.load()?;
        settings.separate_chatgpt();
        if settings.providers.is_empty() {
            settings = previous;
        }
        crate::application::validate(&settings)?;
        store.save(&settings)?;
        database.save(&path)?;
        anyhow::Ok((store, settings))
    })
    .await
    .map_err(failure)?
    .map_err(failure)?;
    app.install_store(store, settings).map_err(failure)?;
    app.configured
        .store(true, std::sync::atomic::Ordering::Release);
    Ok(Json(json!({"configured":true})))
}
#[derive(serde::Deserialize)]
struct TraceQuery {
    after: Option<String>,
}
async fn list_traces(
    State(app): State<Arc<Control>>,
    Query(query): Query<TraceQuery>,
) -> Result<Json<Value>, Failure> {
    let traces = app
        .traces
        .clone()
        .ok_or_else(|| failure("Trazas no disponibles"))?;
    tokio::task::spawn_blocking(move || traces.page(query.after.as_deref()))
        .await
        .map_err(failure)?
        .map(Json)
        .map_err(failure)
}
async fn trace_detail(
    State(app): State<Arc<Control>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    app.traces
        .as_ref()
        .ok_or_else(|| failure("Trazas no disponibles"))?
        .list(Some(&id))
        .map(Json)
        .map_err(failure)
}
async fn clear_traces(State(app): State<Arc<Control>>) -> Result<Json<Value>, Failure> {
    app.traces
        .as_ref()
        .ok_or_else(|| failure("Trazas no disponibles"))?
        .clear()
        .map_err(failure)?;
    Ok(Json(json!({"cleared":true})))
}
async fn settings(State(app): State<Arc<Control>>) -> Json<Value> {
    let settings = app.settings();
    let mut value = serde_json::to_value(&settings).unwrap();
    for (p, stored) in value["providers"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(&settings.providers)
    {
        p["has_api_key"] = json!(stored.api_key.as_ref().is_some_and(|s| !s.is_empty()));
        p["api_key"] = Value::Null;
        p["subscription"] = Value::Null;
        p["subscription_account"] = json!(stored.subscription.as_ref().map(|s| &s.email));
        p["subscription_connected"] = json!(stored.subscription.is_some());
        p["zed"] = Value::Null;
        p["zed_connected"] = json!(stored.zed.is_some());
        p["zed_organization"] = json!(stored.zed.as_ref().map(|s| &s.organization_id));
    }
    Json(value)
}
async fn save(
    State(app): State<Arc<Control>>,
    Json(settings): Json<Settings>,
) -> Result<Json<Value>, Failure> {
    app.replace(settings).await.map_err(failure)?;
    Ok(Json(json!({"saved":true})))
}

#[derive(serde::Deserialize)]
struct CatalogRequest {
    provider: crate::domain::Provider,
    #[serde(default)]
    refresh: bool,
}
async fn provider_catalog(
    State(app): State<Arc<Control>>,
    Json(mut request): Json<CatalogRequest>,
) -> Result<Json<Value>, Failure> {
    crate::application::validate_url(&request.provider.base_url).map_err(failure)?;
    let credential = if request.provider.kind == crate::domain::ProviderKind::Chatgpt {
        crate::domain::Credential::Subscription
    } else {
        crate::domain::Credential::ApiKey
    };
    if credential == crate::domain::Credential::Subscription {
        oauth::refresh_subscription(&app, &request.provider.id)
            .await
            .map_err(failure)?;
    }
    let settings = app.settings();
    let stored = settings
        .providers
        .iter()
        .find(|p| p.id == request.provider.id && p.kind == request.provider.kind);
    if request.provider.api_key.is_none() {
        request.provider.api_key = stored.and_then(|p| p.api_key.clone());
    }
    // OAuth credentials are exclusively managed on the server, including discovery.
    request.provider.subscription = stored.and_then(|p| p.subscription.clone());
    request.provider.zed = stored
        .filter(|p| p.base_url == request.provider.base_url)
        .and_then(|p| p.zed.clone());
    crate::catalog::CATALOG
        .discover(&request.provider, credential, request.refresh)
        .await
        .map(Json)
        .map_err(failure)
}
async fn metadata_providers() -> Result<Json<Value>, Failure> {
    let catalog = crate::catalog::CATALOG
        .metadata(false)
        .await
        .map_err(failure)?;
    let mut providers: Vec<_> = catalog["providers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, p)| json!({"id":id,"name":p["name"].as_str().unwrap_or(id)}))
        .collect();
    providers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(Json(json!({"data":providers})))
}

async fn zed_start(
    State(app): State<Arc<Control>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    let port = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|host| reqwest::Url::parse(&format!("http://{host}")).ok())
        .and_then(|url| url.port())
        .unwrap_or(10801);
    crate::zed::start(&app, &id, port)
        .await
        .map(|url| Json(json!({"url":url})))
        .map_err(failure)
}
async fn zed_connect(
    State(app): State<Arc<Control>>,
    Path(id): Path<String>,
    Json(input): Json<crate::zed::AccountInput>,
) -> Result<Json<Value>, Failure> {
    crate::zed::connect(&app, &id, input)
        .await
        .map_err(failure)?;
    Ok(Json(json!({"connected":true})))
}
async fn zed_disconnect(
    State(app): State<Arc<Control>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    crate::zed::disconnect(&app, &id).await.map_err(failure)?;
    Ok(Json(json!({"disconnected":true,"remote_revoked":false})))
}
async fn zed_callback(
    State(app): State<Arc<Control>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(user_id) = query.get("user_id").and_then(|v| v.parse::<u32>().ok()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(token) = query.get("access_token") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match crate::zed::callback(&app, user_id, token).await {
        Ok(()) => {
            "Cuenta Zed conectada. Vuelve al CLI y actualiza los proveedores.".into_response()
        }
        Err(error) => failure(error).into_response(),
    }
}
async fn cli_config(State(app): State<Arc<Control>>, headers: HeaderMap) -> Json<Value> {
    let settings = app.settings();
    let mut base = reqwest::Url::parse(&format!(
        "http://{}",
        headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("127.0.0.1")
    ))
    .unwrap();
    let _ = base.set_port(Some(app.port));
    let base = settings
        .public_url
        .clone()
        .unwrap_or_else(|| base.as_str().trim_end_matches('/').into());
    let mut env = json!({});
    if let Some(n) = settings.context_tokens {
        env["CLAUDE_CODE_MAX_CONTEXT_TOKENS"] = json!(n.to_string());
    }
    if let Some(n) = settings.compact_tokens {
        env["CLAUDE_CODE_AUTO_COMPACT_WINDOW"] = json!(n.to_string());
    }
    Json(
        json!({"data":settings.models(),"has_more":false,"anthropic_base_url":base,"default_model":settings.default_model,"env":env}),
    )
}
async fn start_oauth(
    State(app): State<Arc<Control>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let port = reqwest::Url::parse(&format!("http://{host}"))
        .ok()
        .and_then(|u| u.port())
        .ok_or_else(|| failure("No se pudo determinar el puerto de el CLI"))?;
    let url = oauth::start(&app, &id, port).await.map_err(failure)?;
    Ok(Json(json!({"url":url})))
}
async fn callback(
    State(app): State<Arc<Control>>,
    Query(query): Query<oauth::Callback>,
) -> Response {
    match oauth::complete(&app, query).await {
        Ok(()) => "Conexión completada. Vuelve al CLI y actualiza el panel.".into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            format!("No se pudo conectar OpenAI: {error}. Vuelve a el CLI e inténtalo de nuevo."),
        )
            .into_response(),
    }
}
async fn disconnect(
    State(app): State<Arc<Control>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let _guard = app.mutation.lock().await;
    let mut settings = app.settings();
    let p = settings
        .providers
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| failure("Proveedor no encontrado"))?;
    let mut revoked = true;
    if let Some(s) = &p.subscription {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(failure)?;
        // Discover the revocation endpoint from the trusted issuer.
        let result = async {
            let discovery: Value = client
                .get("https://auth.openai.com/.well-known/openid-configuration")
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let endpoint = discovery["revocation_endpoint"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Falta endpoint de revocación"))?;
            anyhow::ensure!(
                endpoint.starts_with("https://auth.openai.com/"),
                "Emisor no válido"
            );
            client
                .post(endpoint)
                .form(&[
                    ("token", s.refresh_token.as_str()),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", s.client_id.as_str()),
                ])
                .send()
                .await?
                .error_for_status()?;
            anyhow::Ok(())
        }
        .await;
        revoked = result.is_ok();
    }
    p.subscription = None;
    app.save(settings).await.map_err(failure)?;
    Ok(Json(json!({"disconnected":true,"remote_revoked":revoked})))
}
async fn clear_key(
    State(app): State<Arc<Control>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let _guard = app.mutation.lock().await;
    let mut settings = app.settings();
    settings
        .providers
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| failure("Proveedor no encontrado"))?
        .api_key = None;
    app.save(settings).await.map_err(failure)?;
    Ok(Json(json!({"cleared":true})))
}

struct Gateway {
    app: Arc<Control>,
    route: std::sync::Mutex<Option<(u64, Router)>>,
}
pub fn gateway_with_control(app: Arc<Control>) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(Arc::new(Gateway {
            app,
            route: std::sync::Mutex::new(None),
        }))
}
async fn dispatch(State(gateway): State<Arc<Gateway>>, mut request: Request) -> Response {
    let app = &gateway.app;
    if matches!(
        request.uri().path(),
        "/v1/messages" | "/v1/messages/count_tokens"
    ) && request.method() == axum::http::Method::POST
    {
        let (parts, body) = request.into_parts();
        let bytes = match to_bytes(body, 32 * 1024 * 1024).await {
            Ok(b) => b,
            Err(e) => return failure(e).into_response(),
        };
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && let Err(error) = oauth::refresh(app, value["model"].as_str().unwrap_or("")).await
        {
            return (StatusCode::BAD_GATEWAY, Json(json!({"type":"error","error":{"type":"authentication_error","message":error.to_string()}}))).into_response();
        }
        request = Request::from_parts(parts, Body::from(bytes));
    }
    let route = {
        let mut cached = gateway.route.lock().unwrap();
        let (settings, revision) = app.snapshot();
        if cached.as_ref().is_none_or(|(old, _)| *old != revision) {
            let router =
                match crate::server::configured_router_with_traces(&settings, app.traces.clone()) {
                    Ok(router) => router,
                    Err(error) => return failure(error).into_response(),
                };
            *cached = Some((revision, router));
        }
        cached.as_ref().unwrap().1.clone()
    };
    route.oneshot(request).await.unwrap()
}
