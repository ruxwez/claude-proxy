use crate::{
    application::Application,
    domain::{Settings, SettingsStore},
    management, oauth,
    sqlite::SqliteStore,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type Captured = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, task)
}
fn configured(base: &str) -> Settings {
    serde_json::from_value(json!({"providers":[{"id":"custom","name":"Mi API","kind":"openai_compatible","base_url":base,"api_key":"secret-key","models":[{"id":"small","display_name":"Pequeño","protocol":"chat"},{"id":"hidden","protocol":"chat","enabled":false}]}],"default_model":"custom/small"})).unwrap()
}

#[tokio::test]
async fn sqlite_configuration_is_live_durable_redacted_and_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.db");
    let store = Arc::new(SqliteStore::open(&path).unwrap());
    let app = Arc::new(Application::open(store.clone(), 10800).unwrap());
    let captured = Arc::new(Mutex::new(vec![]));
    let upstream=Router::new().route("/chat/completions",post(|State(c):State<Captured>,h:HeaderMap,Json(body):Json<Value>| async move {
        c.lock().unwrap().push((h,body));
        Json(json!({"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}))
    })).with_state(captured.clone());
    let (up, up_task) = serve(upstream).await;
    let (ui, ui_task) = serve(management::router(app.clone())).await;
    let (gateway, gateway_task) = serve(management::gateway(app.clone())).await;
    let http = reqwest::Client::new();
    for path in [
        "/",
        "/providers",
        "/providers/custom",
        "/settings",
        "/assets/app.js",
        "/assets/style.css",
    ] {
        assert_eq!(
            http.get(format!("{ui}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let preferences: Value = http
        .get(format!("{ui}/api/cli-config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        preferences["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"],
        "990000"
    );
    assert_eq!(
        preferences["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"],
        "900000"
    );
    assert_eq!(
        http.get(format!("{gateway}/api/settings"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        http.post(format!("{ui}/v1/messages"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let before: Value = http
        .get(format!("{gateway}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before["data"], json!([]));
    assert!(
        http.put(format!("{ui}/api/settings"))
            .json(&configured(&up))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let models: Value = http
        .get(format!("{gateway}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"].as_array().unwrap().len(), 1);
    assert_eq!(models["data"][0]["display_name"], "Pequeño");
    let message = json!({"model":"custom/small","max_tokens":16,"messages":[{"role":"user","content":"hello"}]});
    assert!(
        http.post(format!("{gateway}/v1/messages"))
            .header("authorization", "Bearer client-secret")
            .json(&message)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        captured.lock().unwrap()[0].0["authorization"],
        "Bearer secret-key"
    );
    assert_eq!(captured.lock().unwrap()[0].1["model"], "small");
    for model in ["custom/hidden", "custom/arbitrary"] {
        let mut message = message.clone();
        message["model"] = json!(model);
        assert_eq!(
            http.post(format!("{gateway}/v1/messages"))
                .json(&message)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let raw = http
        .get(format!("{ui}/api/settings"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!raw.contains("secret-key"));
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["providers"][0]["has_api_key"], true);
    value["providers"][0]["models"][0]["id"] = json!("renamed");
    value["default_model"] = json!("custom/renamed");
    assert!(
        http.put(format!("{ui}/api/settings"))
            .json(&value)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        store.load().unwrap().providers[0].api_key.as_deref(),
        Some("secret-key")
    );
    assert_eq!(
        http.post(format!("{gateway}/v1/messages"))
            .json(&message)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let restarted = Application::open(Arc::new(SqliteStore::open(&path).unwrap()), 10800).unwrap();
    assert_eq!(restarted.settings().models()[0]["id"], "custom/renamed");
    assert_eq!(restarted.settings().host_id, app.settings().host_id);
    let mut invalid = value.clone();
    invalid["providers"][0]["base_url"] = json!("file:///etc/passwd");
    assert_eq!(
        http.put(format!("{ui}/api/settings"))
            .json(&invalid)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(store.load().unwrap().providers[0].base_url, up);
    value["providers"][0]["api_key"] = json!("");
    assert!(
        http.put(format!("{ui}/api/settings"))
            .json(&value)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        store.load().unwrap().providers[0].api_key.as_deref(),
        Some("")
    );
    ui_task.abort();
    gateway_task.abort();
    up_task.abort();
}

#[tokio::test]
async fn admin_rejects_cross_origin_and_rebinding_requests() {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(
        Application::open(
            Arc::new(SqliteStore::open(&dir.path().join("db")).unwrap()),
            10800,
        )
        .unwrap(),
    );
    let (ui, task) = serve(management::router(app)).await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .put(format!("{ui}/api/settings"))
            .header("origin", "https://attacker.example")
            .json(&Settings::default())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("{ui}/api/settings"))
            .header("host", "attacker.example")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("{ui}/api/settings"))
            .header("sec-fetch-site", "cross-site")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(format!("{ui}/api/providers/openai/oauth"))
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // An OAuth navigation is allowed through the browser guard, then rejected by state validation.
    assert_eq!(
        client
            .get(format!(
                "{ui}/auth/callback?state=invalid&error=access_denied"
            ))
            .header("sec-fetch-site", "cross-site")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    task.abort();
}

#[tokio::test]
async fn oauth_uses_pkce_one_time_state_and_preserves_credentials_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let app = Application::open(
        Arc::new(SqliteStore::open(&dir.path().join("db")).unwrap()),
        10800,
    )
    .unwrap();
    let mut settings = configured("https://api.openai.com/v1");
    settings.providers[0].kind = crate::domain::ProviderKind::Chatgpt;
    settings.providers[0].api_key = None;
    for m in &mut settings.providers[0].models {
        m.credential = crate::domain::Credential::Subscription;
        m.protocol = crate::domain::Protocol::Responses;
    }
    app.replace(settings).await.unwrap();
    let url = reqwest::Url::parse(&oauth::start(&app, "custom", 10801).await.unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(
        query["redirect_uri"],
        "http://127.0.0.1:10801/auth/callback"
    );
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["code_challenge"].len() >= 43);
    let callback = || {
        serde_json::from_value::<oauth::Callback>(
            json!({"state":query["state"],"error":"access_denied"}),
        )
        .unwrap()
    };
    assert!(oauth::complete(&app, callback()).await.is_err());
    assert!(oauth::complete(&app, callback()).await.is_err());
    assert!(app.settings().providers[0].subscription.is_none());
    assert!(app.settings().providers[0].api_key.is_none());
}

#[test]
fn rejects_duplicate_models_bad_defaults_and_incompatible_credentials() {
    let mut settings = configured("https://example.com/v1");
    assert!(settings.validate().is_ok());
    settings.providers[0].models[0].credential = crate::domain::Credential::Subscription;
    assert!(settings.validate().is_err());
    settings.providers[0].models[0].credential = crate::domain::Credential::ApiKey;
    settings.default_model = Some("custom/hidden".into());
    assert!(settings.validate().is_err());
    settings.default_model = None;
    let duplicate = settings.providers[0].models[0].clone();
    settings.providers[0].models.push(duplicate);
    assert!(settings.validate().is_err());
}

#[test]
fn google_tool_signatures_survive_json_streaming_and_history() {
    use crate::{domain::Protocol, stream::Translator, translate};
    let model = "google/gemini-custom";
    let extra = json!({"google":{"thought_signature":"opaque-google-signature"}});
    let call = json!({"id":"call1","type":"function","extra_content":extra,"function":{"name":"read","arguments":"{}"}});
    let raw = json!({"choices":[{"message":{"tool_calls":[call]},"finish_reason":"tool_calls"}]});
    let response = translate::response(&raw, model, Protocol::Chat).unwrap();
    let mut stream = Translator::with_model(Protocol::Chat, model);
    let mut call = call.clone();
    call["index"] = json!(0);
    let events = stream
        .accept(
            &json!({"choices":[{"delta":{"tool_calls":[call]},"finish_reason":"tool_calls"}]})
                .to_string(),
        )
        .unwrap();
    let state = events
        .iter()
        .find(|e| e["content_block"]["type"] == "redacted_thinking")
        .unwrap()["content_block"]
        .clone();
    for content in [
        response["content"].clone(),
        json!([state,{"type":"tool_use","id":"call1","name":"read","input":{}}]),
    ] {
        let request = json!({"model":model,"max_tokens":32,"messages":[{"role":"assistant","content":content},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call1","content":"done"}]}]});
        let body = translate::request(&request, "gemini-custom", Protocol::Chat).unwrap();
        assert_eq!(body["messages"][0]["tool_calls"][0]["extra_content"], extra);
        let mut changed = request.clone();
        changed["model"] = json!("other/gemini-custom");
        let body = translate::request(&changed, "gemini-custom", Protocol::Chat).unwrap();
        assert!(
            body["messages"][0]["tool_calls"][0]
                .get("extra_content")
                .is_none()
        );
    }
}
#[test]
fn combined_openai_settings_are_split_durably_without_losing_secrets_or_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(&dir.path().join("db")).unwrap());
    let legacy: Settings = serde_json::from_value(json!({"providers":[
        {"id":"openai","name":"OpenAI","kind":"openai","base_url":"https://api.openai.com/v1","api_key":"private-api-key",
         "subscription":{"client_id":"client","subject":"subject","email":"user@example.test","access_token":"private-access","refresh_token":"private-refresh","id_token":"private-id","expires_at":4000000000u64},
         "models":[{"id":"api-model","protocol":"responses","credential":"api_key"},{"id":"plan/model","protocol":"responses","credential":"subscription"}]},
        {"id":"openai-chatgpt","name":"Existing","kind":"openai_compatible","base_url":"https://example.test/v1","models":[]}
    ],"default_model":"openai/plan/model"})).unwrap();
    store.save(&legacy).unwrap();
    let app = Application::open(store.clone(), 10800).unwrap();
    let settings = app.settings();
    assert_eq!(settings.providers.len(), 3);
    let api = &settings.providers[0];
    assert!(api.kind == crate::domain::ProviderKind::Openai);
    assert_eq!(api.api_key.as_deref(), Some("private-api-key"));
    assert!(api.subscription.is_none());
    assert_eq!(api.models.len(), 1);
    let plan = &settings.providers[2];
    assert!(plan.kind == crate::domain::ProviderKind::Chatgpt);
    assert_eq!(plan.id, "openai-chatgpt-2");
    assert!(plan.api_key.is_none());
    assert_eq!(
        plan.subscription.as_ref().unwrap().refresh_token,
        "private-refresh"
    );
    assert_eq!(plan.models[0].id, "plan/model");
    assert_eq!(
        settings.default_model.as_deref(),
        Some("openai-chatgpt-2/plan/model")
    );
    assert!(settings.validate().is_ok());
    let reopened = Application::open(store, 10800).unwrap();
    assert_eq!(reopened.settings().providers.len(), 3);
    assert_eq!(reopened.settings().default_model, settings.default_model);
}

#[test]
fn subscription_only_migration_preserves_public_ids_and_mixed_credentials_are_rejected() {
    let mut settings: Settings = serde_json::from_value(json!({"providers":[{"id":"account","name":"Old plan","kind":"openai","base_url":"https://api.openai.com/v1","models":[{"id":"model","protocol":"responses","credential":"subscription"}]}],"default_model":"account/model"})).unwrap();
    assert!(settings.separate_chatgpt());
    assert!(settings.validate().is_ok());
    assert_eq!(settings.default_model.as_deref(), Some("account/model"));
    assert!(!settings.separate_chatgpt());
    settings.providers[0].api_key = Some("forbidden-key".into());
    assert!(settings.validate().is_err());
    settings.providers[0].api_key = None;
    settings.providers[0].models[0].credential = crate::domain::Credential::ApiKey;
    assert!(settings.validate().is_err());
}
