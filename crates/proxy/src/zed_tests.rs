use crate::{
    application::Application,
    domain::{Settings, SettingsStore, ZedCredentials},
    sqlite::SqliteStore,
};
use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
pub(crate) struct Mock {
    calls: Mutex<Vec<(String, HeaderMap, Value)>>,
    tokens: Mutex<usize>,
}
// Mirror the two constraints of Zed's anthropic::Message that differ from
// permissive JSON forwarding. This fixture must reject the reported request.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum ZedRole {
    User,
    Assistant,
}
#[derive(serde::Deserialize, serde::Serialize)]
struct ZedMessage {
    role: ZedRole,
    content: Vec<Value>,
}
#[test]
fn zed_wire_schema_reproduces_system_role_and_string_content_errors() {
    let bad = json!([{"role":"system","content":[{"type":"text","text":"instructions"}]}]);
    let error = serde_json::from_value::<Vec<ZedMessage>>(bad)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("unknown variant `system`"), "{error}");
    assert!(
        serde_json::from_value::<Vec<ZedMessage>>(json!([{"role":"user","content":"hello"}]))
            .is_err()
    );
}
async fn mock(
    State(state): State<Arc<Mock>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|v| v.0).unwrap_or(Value::Null);
    state
        .calls
        .lock()
        .unwrap()
        .push((uri.path().into(), headers.clone(), body.clone()));
    match uri.path() {
        "/client/users/me" => {
            assert_eq!(headers["authorization"], "42 account-secret");
            Json(json!({"user":{"legacy_user_id":42},"default_organization_id":"org","organizations":[{"id":"org","is_personal":true}]})).into_response()
        }
        "/client/llm_tokens" => {
            assert_eq!(headers["authorization"], "42 account-secret");
            assert_eq!(body, json!({"organization_id":"org"}));
            let mut n = state.tokens.lock().unwrap();
            *n += 1;
            Json(json!({"token":format!("llm-{n}")})).into_response()
        }
        "/models" => {
            assert!(
                headers["authorization"]
                    .to_str()
                    .unwrap()
                    .starts_with("Bearer llm-")
            );
            Json(json!({"models":[{"id":"claude","display_name":"Claude","provider":"anthropic"},{"id":"gpt","provider":"open_ai"},{"id":"disabled","provider":"google","is_disabled":true}]})).into_response()
        }
        "/completions" => {
            assert_eq!(headers["x-zed-version"], crate::zed::VERSION);
            assert_eq!(
                headers["x-zed-client-supports-stream-ended-request-completion-status"],
                "true"
            );
            assert!(!headers.contains_key("x-api-key"));
            assert!(
                !headers["authorization"]
                    .to_str()
                    .unwrap()
                    .contains("client-secret")
            );
            if headers["authorization"] == "Bearer llm-1" {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let events = match body["provider"].as_str().unwrap() {
                "anthropic" => {
                    if let Err(error) = serde_json::from_value::<Vec<ZedMessage>>(
                        body["provider_request"]["messages"].clone(),
                    ) {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("failed to parse Anthropic request: {error}"),
                        )
                            .into_response();
                    }
                    assert_eq!(body["provider_request"]["stream"], true);
                    vec![
                        json!({"type":"message_start","message":{"id":"msg_native","type":"message","role":"assistant","model":"claude","content":[],"usage":{"input_tokens":7,"output_tokens":0,"cache_read_input_tokens":4}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hola 🦀"}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
                        json!({"type":"message_stop"}),
                    ]
                }
                "open_ai" | "x_ai" => vec![
                    json!({"type":"response.output_text.delta","delta":"hola 🦀"}),
                    json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hola 🦀"}]}],"usage":{"input_tokens":7,"output_tokens":2}}}),
                ],
                "google" => {
                    assert!(
                        body["provider_request"]["model"]
                            .as_str()
                            .unwrap()
                            .starts_with("models/")
                    );
                    vec![
                        json!({"candidates":[{"index":0,"content":{"parts":[{"text":"hola 🦀"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":2}}),
                    ]
                }
                _ => unreachable!(),
            };
            let mut wire = format!("{}\n", json!({"status":"started"}));
            for event in events {
                wire.push_str(&format!("{}\n", json!({"event":event})));
            }
            match body["model"].as_str().unwrap() {
                "truncated" => {},
                "failed" => wire.push_str(&format!("{}\n", json!({"status":{"failed":{"code":"overloaded","message":"busy","retry_after":2}}}))),
                _ => wire.push_str(&format!("{}\n", json!({"status":"stream_ended"}))),
            }
            (
                [
                    ("content-type", "application/x-ndjson"),
                    ("x-zed-server-supports-status-messages", "true"),
                ],
                wire,
            )
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }),
    )
}
pub(crate) async fn upstream() -> (String, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let mock_state = Arc::new(Mock::default());
    let (base, task) = serve(
        Router::new()
            .route("/client/users/me", get(mock))
            .route("/client/llm_tokens", post(mock))
            .route("/models", get(mock))
            .route("/completions", post(mock))
            .with_state(mock_state.clone()),
    )
    .await;
    (base, mock_state, task)
}
fn credentials() -> ZedCredentials {
    ZedCredentials {
        user_id: 42,
        access_token: "account-secret".into(),
        organization_id: "org".into(),
        system_id: "system".into(),
        client_version: crate::zed::VERSION.into(),
    }
}
fn request(model: &str, stream: bool) -> Value {
    json!({"model":format!("zed/{model}"),"max_tokens":128,"stream":stream,"messages":[{"role":"user","content":"hola"}]})
}

#[tokio::test]
async fn completions_are_anthropic_for_all_native_providers_and_both_client_transports() {
    use claude_proxy_engine::zed::Provider;
    let (up, state, up_task) = upstream().await;
    let models = HashMap::from([
        ("claude".into(), Provider::Anthropic),
        ("gpt".into(), Provider::OpenAi),
        ("grok".into(), Provider::XAi),
        ("gemini".into(), Provider::Google),
        ("failed".into(), Provider::Anthropic),
        ("truncated".into(), Provider::Anthropic),
    ]);
    let protocols = models
        .iter()
        .map(|(id, p)| (format!("zed/{id}"), p.protocol()))
        .collect();
    let config = crate::Config {
        catalog: None,
        port: 1,
        timeout: Duration::from_secs(5),
        client_version: "test".into(),
        project_id: "test".into(),
        protocols,
        providers: vec![crate::ProviderConfig {
            zed: Some(crate::zed::Config {
                credentials: Some(credentials()),
                models,
            }),
            name: "zed".into(),
            base: up,
            key: String::new(),
            opencode: false,
            free_only: false,
            models: vec![],
        }],
    };
    let (base, task) =
        serve(crate::server::router_with_subscriptions(config, Default::default()).unwrap()).await;
    let http = reqwest::Client::new();
    for model in ["claude", "gpt", "grok", "gemini"] {
        for stream in [false, true] {
            let mut message = request(model, stream);
            if model == "claude" {
                message["system"] = json!("instrucción global");
                message["messages"].as_array_mut().unwrap().insert(0,json!({"role":"system","content":[{"type":"text","text":"regla extra","cache_control":{"type":"ephemeral"}}]}));
            }
            let response = http
                .post(format!("{base}/v1/messages"))
                .bearer_auth("client-secret")
                .json(&message)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            if stream {
                assert_eq!(response.headers()["content-type"], "text/event-stream");
                let body = response.text().await.unwrap();
                assert!(
                    body.contains("event: message_start") && body.contains("event: message_stop"),
                    "{body}"
                );
                assert!(body.contains("hola 🦀"));
                assert!(!body.contains("event: error"), "{body}");
            } else {
                let value: Value = response.json().await.unwrap();
                assert_eq!(value["type"], "message");
                assert_eq!(value["model"], format!("zed/{model}"));
                assert_eq!(value["content"][0]["text"], "hola 🦀");
                assert_eq!(value["stop_reason"], "end_turn");
                assert_eq!(value["usage"]["input_tokens"], 7);
                assert_eq!(value["usage"]["output_tokens"], 2);
            }
        }
    }
    assert_eq!(
        *state.tokens.lock().unwrap(),
        2,
        "initial exchange plus one rejected-token refresh; reused thereafter"
    );
    for model in ["truncated", "failed"] {
        let response = http
            .post(format!("{base}/v1/messages"))
            .json(&request(model, false))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let value: Value = response.json().await.unwrap();
        assert_eq!(value["type"], "error");
        let response = http
            .post(format!("{base}/v1/messages"))
            .json(&request(model, true))
            .send()
            .await
            .unwrap();
        let body = response.text().await.unwrap();
        assert!(body.contains("event: error"), "{body}");
        assert!(
            !body.contains("event: message_stop"),
            "a truncated stream must not appear completed"
        );
    }
    let response = http
        .post(format!("{base}/v1/messages/count_tokens"))
        .json(&request("claude", false))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers()["x-claude-proxy-token-count"],
        "estimated"
    );
    let calls = state.calls.lock().unwrap();
    for (_, _, body) in calls
        .iter()
        .filter(|(path, _, body)| path == "/completions" && body["model"] == "claude")
    {
        assert_eq!(
            body["provider_request"]["system"][0]["text"],
            "instrucción global"
        );
        assert_eq!(
            body["provider_request"]["system"][1]["cache_control"],
            json!({"type":"ephemeral"})
        );
    }
    assert!(
        calls
            .iter()
            .all(|(path, _, _)| path == "/client/llm_tokens" || path == "/completions")
    );
    task.abort();
    up_task.abort();
}

#[tokio::test]
async fn account_is_server_managed_durable_and_discoverable_from_cli_api() {
    let (up, state, up_task) = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.db");
    let store = Arc::new(SqliteStore::open(&path).unwrap());
    let settings: Settings = serde_json::from_value(json!({"providers":[{"id":"zed","name":"Zed","kind":"zed","base_url":up,"models":[{"id":"claude","protocol":"messages","zed_provider":"anthropic"}]}]})).unwrap();
    store.save(&settings).unwrap();
    let app = Arc::new(Application::open(store, 10800).unwrap());
    let (base, task) = serve(crate::management::router(app.clone())).await;
    let http = reqwest::Client::new();
    let connected = http
        .post(format!("{base}/api/providers/zed/zed/credentials"))
        .json(&json!({"user_id":42,"access_token":"account-secret"}))
        .send()
        .await
        .unwrap();
    assert!(
        connected.status().is_success(),
        "{}",
        connected.text().await.unwrap()
    );
    let mut public: Value = http
        .get(format!("{base}/api/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(public["providers"][0]["zed_connected"], true);
    assert!(public["providers"][0]["zed"].is_null());
    assert!(!public.to_string().contains("account-secret"));
    public["providers"][0]["zed"] = json!({"user_id":99,"access_token":"forged-secret","organization_id":"forged","system_id":"forged","client_version":"forged"});
    assert!(
        http.put(format!("{base}/api/settings"))
            .json(&public)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        app.settings().providers[0]
            .zed
            .as_ref()
            .unwrap()
            .access_token,
        "account-secret"
    );
    let persisted = SqliteStore::open(&path).unwrap().load().unwrap();
    assert_eq!(
        persisted.providers[0].zed.as_ref().unwrap().access_token,
        "account-secret"
    );
    let catalog: Value = http
        .post(format!("{base}/api/providers/catalog"))
        .json(&json!({"provider":public["providers"][0]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(catalog["metadata_source"], "zed", "{catalog}");
    assert_eq!(catalog["data"].as_array().unwrap().len(), 2);
    assert_eq!(catalog["data"][0]["zed_provider"], "anthropic");
    assert_eq!(catalog["data"][1]["suggested_protocol"], "responses");
    assert!(
        state
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _, _)| path == "/models")
    );
    assert!(
        http.delete(format!("{base}/api/providers/zed/zed/credentials"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(app.settings().providers[0].zed.is_none());
    assert!(
        SqliteStore::open(&path).unwrap().load().unwrap().providers[0]
            .zed
            .is_none()
    );
    let (gateway, gateway_task) = serve(crate::management::gateway(app)).await;
    let response = http
        .post(format!("{gateway}/v1/messages"))
        .json(&request("claude", false))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    gateway_task.abort();
    task.abort();
    up_task.abort();
}

#[tokio::test]
async fn concurrent_rejections_share_a_single_token_refresh() {
    let (up, state, task) = upstream().await;
    let connection = Arc::new(crate::zed::Connection::new(
        &up,
        credentials(),
        reqwest::Client::new(),
    ));
    let futures = (0..8).map(|_| { let connection = connection.clone(); async move { connection.send(reqwest::Method::POST,"/completions",Some(&json!({"provider":"anthropic","model":"claude","provider_request":{"stream":true,"messages":[{"role":"user","content":[{"type":"text","text":"hola"}]}]}}))).await.unwrap().status() } });
    for status in futures_util::future::join_all(futures).await {
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(*state.tokens.lock().unwrap(), 2);
    task.abort();
}
