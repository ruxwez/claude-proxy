use crate::domain::{Credential, Provider, ProviderKind};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub static CATALOG: LazyLock<Catalog> =
    LazyLock::new(|| Catalog::new("https://models.dev/catalog.json?type=all"));

pub struct Catalog {
    metadata_url: String,
    cache: Mutex<Option<(Instant, Value)>>,
}
impl Catalog {
    pub fn new(metadata_url: &str) -> Self {
        Self {
            metadata_url: metadata_url.into(),
            cache: Mutex::new(None),
        }
    }
    fn client() -> Result<Client> {
        Ok(Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("claude-proxy/0.1")
            .redirect(reqwest::redirect::Policy::none())
            .build()?)
    }
    pub async fn metadata(&self, refresh: bool) -> Result<Value> {
        let mut cache = self.cache.lock().await;
        if !refresh
            && let Some((time, data)) = &*cache
            && time.elapsed() < Duration::from_secs(900)
        {
            return Ok(data.clone());
        }
        // Deliberately separate request: never attach a provider credential here.
        let data = read_json(Self::client()?.get(&self.metadata_url)).await?;
        ensure!(
            data["providers"].is_object() && data["models"].is_object(),
            "models.dev devolvió un catálogo inválido"
        );
        *cache = Some((Instant::now(), data.clone()));
        Ok(data)
    }
    pub async fn discover(
        &self,
        provider: &Provider,
        credential: Credential,
        refresh: bool,
    ) -> Result<Value> {
        let upstream = fetch_models(provider, credential).await?;
        let metadata_provider = provider
            .models_dev_provider
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(match provider.kind {
                ProviderKind::OpencodeGo => "opencode-go",
                ProviderKind::Openai | ProviderKind::Chatgpt => "openai",
                ProviderKind::Google => "google",
                ProviderKind::OpenaiCompatible => &provider.id,
            });
        let (catalog, warning) = match self.metadata(refresh).await {
            Ok(data) => (data, None),
            Err(_) => (
                json!({"providers":{},"models":{}}),
                Some(
                    "No se pudo consultar models.dev. Se muestran los modelos del proveedor sin cotejar; vuelve a consultar el catálogo.",
                ),
            ),
        };
        let data: Vec<_> = upstream
            .into_iter()
            .map(|raw| {
                let id = raw["id"].as_str().unwrap();
                let record = &catalog["providers"][metadata_provider]["models"][id];
                let canonical_id = record["canonical_model_id"]
                    .as_str()
                    .or_else(|| catalog["models"].get(id).map(|_| id));
                let canonical = canonical_id
                    .map(|id| &catalog["models"][id])
                    .filter(|v| v.is_object());
                let mut metadata = canonical.cloned().unwrap_or(Value::Null);
                if record.is_object() {
                    merge(&mut metadata, record);
                }
                let matched = if record.is_object() {
                    "exact"
                } else if canonical.is_some() {
                    "canonical"
                } else {
                    "none"
                };
                let protocol = if credential == Credential::Subscription
                    || provider.kind == ProviderKind::Openai
                {
                    crate::domain::Protocol::Responses
                } else if provider.kind == ProviderKind::OpencodeGo {
                    crate::provider::protocol(id)
                } else {
                    crate::domain::Protocol::Chat
                };
                json!({"id":id,"display_name":metadata["name"].as_str().unwrap_or(id),
                "suggested_protocol":protocol,"metadata":metadata,
                "match":matched,"canonical_model_id":canonical_id})
            })
            .collect();
        Ok(
            json!({"data":data,"metadata_provider":metadata_provider,"metadata_source":self.metadata_url,"warning":warning}),
        )
    }
}

// Only explicit canonical links supply missing fields. Provider-specific values win.
fn merge(target: &mut Value, source: &Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            merge(target.entry(key).or_insert(Value::Null), value);
        }
    } else {
        *target = source.clone();
    }
}

async fn read_json(request: RequestBuilder) -> Result<Value> {
    let mut response = request
        .header("accept", "application/json")
        .send()
        .await
        .context("No se pudo conectar con el catálogo")?;
    ensure!(
        response.status().is_success(),
        "El catálogo devolvió HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 32 * 1024 * 1024,
            "Catálogo demasiado grande"
        );
        bytes.extend(chunk);
    }
    serde_json::from_slice(&bytes).context("El catálogo no es JSON válido")
}

async fn fetch_models(provider: &Provider, credential: Credential) -> Result<Vec<Value>> {
    let http = Catalog::client()?;
    let (base, key) = if credential == Credential::Subscription {
        ensure!(
            provider.kind == ProviderKind::Chatgpt,
            "La suscripción solo está disponible para ChatGPT"
        );
        (
            "https://api.openai.com/v1",
            provider
                .subscription
                .as_ref()
                .context("Conecta la suscripción OpenAI primero")?
                .access_token
                .as_str(),
        )
    } else {
        (
            provider.base_url.trim_end_matches('/'),
            provider.api_key.as_deref().unwrap_or(""),
        )
    };
    let mut url = reqwest::Url::parse(&format!("{base}/models"))?;
    let origin = url.origin();
    let mut visited = HashSet::new();
    let mut models = BTreeMap::new();
    for _ in 0..100 {
        ensure!(
            visited.insert(url.to_string()),
            "El proveedor repite una página del catálogo"
        );
        let mut request = http.get(url.clone());
        if !key.is_empty() {
            request = request.bearer_auth(key);
        }
        if provider.kind == ProviderKind::OpencodeGo {
            request = request
                .header("x-opencode-client", "cli")
                .header("user-agent", "opencode/2.0.26");
        }
        let page = read_json(request).await?;
        let items = page["data"]
            .as_array()
            .context("El catálogo del proveedor debe contener data")?;
        for raw in items {
            let id = raw["id"]
                .as_str()
                .filter(|id| {
                    !id.is_empty() && id.trim() == *id && !id.chars().any(char::is_control)
                })
                .context("El catálogo contiene un ID inválido")?;
            models
                .entry(id.to_string())
                .and_modify(|previous| merge(previous, raw))
                .or_insert_with(|| raw.clone());
        }
        if let Some(next) = page["next_page"].as_str().filter(|s| !s.is_empty()) {
            let next = url.join(next)?;
            ensure!(
                next.origin() == origin && next.username().is_empty() && next.password().is_none(),
                "La siguiente página del catálogo cambia de origen"
            );
            url = next;
        } else if page["has_more"] == true {
            ensure!(!items.is_empty(), "Catálogo parcial: página vacía");
            let last = page["last_id"]
                .as_str()
                .or_else(|| items.last().and_then(|m| m["id"].as_str()))
                .context("Catálogo parcial: falta cursor")?;
            url = reqwest::Url::parse(&format!("{base}/models"))?;
            url.query_pairs_mut().append_pair("after", last);
        } else {
            return Ok(models.into_values().collect());
        }
    }
    anyhow::bail!("Demasiadas páginas en el catálogo del proveedor")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Query, State},
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (base, task)
    }
    fn provider(base: &str) -> Provider {
        serde_json::from_value(json!({"id":"local","name":"Local","kind":"openai_compatible","base_url":base,"api_key":"private-key","models_dev_provider":"reference"})).unwrap()
    }
    #[tokio::test]
    async fn compares_paginated_upstream_with_exact_metadata_and_explicit_canonical_links() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app=Router::new().route("/models",get(|headers:HeaderMap,Query(query):Query<HashMap<String,String>>|async move{
            assert_eq!(headers["authorization"],"Bearer private-key");
            Json(if query.contains_key("after") {json!({"data":[{"id":"unknown-contributor"},{"id":"lab/base"},{"id":"small"}],"has_more":false})}
            else {json!({"data":[{"id":"small","limit":{"context":16000}}],"has_more":true,"last_id":"small"})})
        })).route("/metadata",get(|State(calls):State<Arc<AtomicUsize>>,headers:HeaderMap|async move {
            assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("x-api-key"));
            calls.fetch_add(1,Ordering::Relaxed);
            Json(json!({"providers":{"reference":{"models":{"small":{"canonical_model_id":"lab/base","limit":{"context":32000},"cost":{"input":0.1}},"dev-only":{"limit":{"context":1_000_000}}}}},"models":{"lab/base":{"limit":{"context":64000,"output":8000},"tool_call":true,"reasoning":true}}}))
        })).with_state(calls.clone());
        let (base, task) = serve(app).await;
        let catalog = Catalog::new(&format!("{base}/metadata"));
        let result = catalog
            .discover(&provider(&base), Credential::ApiKey, false)
            .await
            .unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 3);
        let small = result["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "small")
            .unwrap();
        // The duplicate does not erase information in the first page.
        assert_eq!(small["metadata"]["limit"]["context"], 32000);
        assert_eq!(small["metadata"]["limit"]["output"], 8000);
        assert_eq!(small["match"], "exact");
        assert!(small.get("provider_limits").is_none());
        assert!(small.get("provider_data").is_none());
        let linked = &result["data"][0];
        assert_eq!(linked["id"], "lab/base");
        assert_eq!(linked["match"], "canonical");
        let unknown = result["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "unknown-contributor")
            .unwrap();
        assert_eq!(unknown["match"], "none");
        assert!(unknown["metadata"].is_null());
        catalog
            .discover(&provider(&base), Credential::ApiKey, false)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        task.abort();
    }
    #[tokio::test]
    async fn uses_only_models_dev_and_keeps_models_when_metadata_is_unavailable() {
        let app=Router::new().route("/models",get(||async {Json(json!({"data":[{"id":"small","context_length":8000,"max_input_tokens":7000,"max_output_tokens":1000}]}))}))
            .route("/metadata",get(||async {Json(json!({"providers":{"reference":{"models":{"small":{"limit":{"context":32000,"input":30000,"output":2000}}}}},"models":{}}))}));
        let (base, task) = serve(app).await;
        let result = Catalog::new(&format!("{base}/metadata"))
            .discover(&provider(&base), Credential::ApiKey, false)
            .await
            .unwrap();
        assert_eq!(result["data"][0]["metadata"]["limit"]["context"], 32000);
        assert!(result["data"][0].get("provider_data").is_none());
        assert!(result["data"][0].get("provider_limits").is_none());
        let missing = Catalog::new(&format!("{base}/missing"))
            .discover(&provider(&base), Credential::ApiKey, false)
            .await
            .unwrap();
        assert_eq!(missing["data"][0]["id"], "small");
        assert!(missing["warning"].is_string());
        assert_eq!(missing["data"][0]["match"], "none");
        task.abort();
    }
    #[tokio::test]
    async fn discovers_every_provider_kind_with_metadata_exclusively_from_models_dev() {
        let app = Router::new()
            .route("/models", get(|| async { Json(json!({"data":[
                {"id":"shared","name":"Wrong API name","limit":{"context":1},"cost":{"input":999},"reasoning":false},
                {"id":"unknown","canonical_model_id":"lab/base","name":"Untrusted API name"}
            ]})) }))
            .route("/metadata", get(|| async { Json(json!({"providers":{
                "opencode-go":{"models":{"shared":{"name":"OpenCode model","limit":{"context":32000}}}},
                "openai":{"models":{"shared":{"name":"OpenAI model","limit":{"context":64000}}}},
                "google":{"models":{"shared":{"name":"Google model","limit":{"context":128000}}}},
                "local":{"models":{"shared":{"name":"Custom model","limit":{"context":16000}}}}
            },"models":{"lab/base":{"name":"Canonical model"}}})) }));
        let (base, task) = serve(app).await;
        let catalog = Catalog::new(&format!("{base}/metadata"));
        for (kind, name, context) in [
            (ProviderKind::OpencodeGo, "OpenCode model", 32000),
            (ProviderKind::Openai, "OpenAI model", 64000),
            (ProviderKind::Google, "Google model", 128000),
            (ProviderKind::OpenaiCompatible, "Custom model", 16000),
        ] {
            let mut provider = provider(&base);
            provider.kind = kind;
            provider.models_dev_provider = None;
            let result = catalog
                .discover(&provider, Credential::ApiKey, false)
                .await
                .unwrap();
            let model = &result["data"][0];
            assert_eq!(model["display_name"], name);
            assert_eq!(model["metadata"]["limit"]["context"], context);
            assert!(model["metadata"]["cost"].is_null());
            assert!(model["metadata"]["reasoning"].is_null());
            assert_eq!(result["data"][1]["display_name"], "unknown");
            assert_eq!(result["data"][1]["match"], "none");
        }
        task.abort();
    }
    #[tokio::test]
    async fn rejects_partial_catalogs_cycles_and_credentials_forwarded_to_another_origin() {
        let cases = [
            json!({"data":[],"has_more":true}),
            json!({"data":[{"id":"a"}],"next_page":"/models"}),
            json!({"data":[{"id":"a"}],"next_page":"https://other.example/models"}),
            json!({"unexpected":[]}),
        ];
        for page in cases {
            let app = Router::new().route(
                "/models",
                get(move || {
                    let page = page.clone();
                    async move { Json(page) }
                }),
            );
            let (base, task) = serve(app).await;
            assert!(
                fetch_models(&provider(&base), Credential::ApiKey)
                    .await
                    .is_err()
            );
            task.abort();
        }
        let (base, task) =
            serve(Router::new().route("/models", get(|| async { StatusCode::UNAUTHORIZED }))).await;
        let error = fetch_models(&provider(&base), Credential::ApiKey)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("401"));
        assert!(!error.contains("private-key"));
        task.abort();
    }
}
