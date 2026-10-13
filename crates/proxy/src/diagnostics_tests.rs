use crate::{
    application::Application,
    database::Database,
    domain::{Settings, SettingsStore},
    management::{self, Control},
    sqlite::SqliteStore,
    traces::{Traces, now},
};
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    (
        endpoint,
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }),
    )
}
async fn recorded(traces: &Traces, count: usize) -> Value {
    for _ in 0..100 {
        let result = traces.list(None).unwrap();
        if result["data"].as_array().unwrap().len() >= count {
            return result;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("La traza no se persistió");
}

#[tokio::test]
async fn remote_admin_accepts_only_explicit_dns_hosts() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(&directory.path().join("settings.db")).unwrap());
    let control = Arc::new(Control {
        auth: crate::auth::Auth::default(),
        application: Arc::new(Application::open(store, 10800).unwrap()),
        allowed_hosts: vec!["proxy.example.com".into()],
        traces: None,
        bootstrap: None,
        configured: AtomicBool::new(true),
    });
    let (endpoint, task) = serve(management::router_with_control(control)).await;
    let http = reqwest::Client::new();
    assert!(
        http.get(format!("{endpoint}/api/status"))
            .header("host", "proxy.example.com")
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        http.get(format!("{endpoint}/api/status"))
            .header("host", "untrusted.example.com")
            .send()
            .await
            .unwrap()
            .status(),
        axum::http::StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn setup_selects_server_database_and_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(&directory.path().join("initial.sqlite3")).unwrap());
    let application = Arc::new(Application::open(store, 10800).unwrap());
    let config = directory.path().join("database.json");
    let control = Arc::new(Control {
        auth: crate::auth::Auth::default(),
        application,
        traces: None,
        bootstrap: Some(config.clone()),
        allowed_hosts: vec![],
        configured: AtomicBool::new(false),
    });
    let (endpoint, task) = serve(management::router_with_control(control)).await;
    let http = reqwest::Client::new();
    let status: Value = http
        .get(format!("{endpoint}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["configured"], false);
    let body = json!({"backend":"sqlite","path":""});
    assert!(
        http.post(format!("{endpoint}/api/setup"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        http.post(format!("{endpoint}/api/setup"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        axum::http::StatusCode::BAD_REQUEST
    );
    let database: Database = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    let store = database.open().unwrap();
    let settings = store.load().unwrap();
    assert_eq!(settings.context_tokens, Some(990000));
    assert!(directory.path().join("proxy.sqlite3").exists());
    task.abort();
}

#[tokio::test]
async fn traces_capture_all_boundaries_and_deepseek_sse_without_sensitive_content() {
    let directory = tempfile::tempdir().unwrap();
    let upstream = Router::new().route("/chat/completions",post(|Json(body):Json<Value>| async move {
        use axum::response::IntoResponse;
        if body["stream"] == true {
            let frames = [json!({"choices":[{"index":0,"delta":{"reasoning_content":"private-thought"}}]}),json!({"choices":[{"index":0,"delta":{"content":"private-answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":5}})];
            let stream = frames.into_iter().map(|v| format!("data: {v}\n\n")).collect::<String>()+"data: [DONE]\n\n";
            ([("content-type","text/event-stream")],stream).into_response()
        } else { Json(json!({"choices":[{"message":{"content":"private-answer","reasoning_content":"private-thought"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":5}})).into_response() }
    }));
    let (upstream, upstream_task) = serve(upstream).await;
    let store = Arc::new(SqliteStore::open(&directory.path().join("settings.sqlite3")).unwrap());
    let settings: Settings = serde_json::from_value(json!({"providers":[{"id":"opencode-go","name":"Go","kind":"opencode_go","base_url":upstream,"api_key":"private-provider-key","models":[{"id":"deepseek-v4.1-flash","protocol":"chat"}]}],"traces":{"enabled":true,"retention_hours":24}})).unwrap();
    store.save(&settings).unwrap();
    let traces = Traces::open(&directory.path().join("traces.sqlite3")).unwrap();
    let control = Arc::new(Control {
        auth: crate::auth::Auth::default(),
        application: Arc::new(Application::open(store, 10800).unwrap()),
        traces: Some(traces.clone()),
        bootstrap: None,
        allowed_hosts: vec![],
        configured: AtomicBool::new(true),
    });
    let (endpoint, task) = serve(management::gateway_with_control(control.clone())).await;
    let (admin, admin_task) = serve(management::router_with_control(control)).await;
    for stream in [false, true] {
        let response = reqwest::Client::new().post(format!("{endpoint}/v1/messages")).json(&json!({"model":"opencode-go/deepseek-v4.1-flash","max_tokens":64,"stream":stream,"thinking":{"type":"enabled"},"messages":[{"role":"user","content":"private-prompt"}]})).send().await.unwrap();
        assert!(response.status().is_success());
        assert!(response.headers().contains_key("x-claude-proxy-trace-id"));
        assert!(response.text().await.unwrap().contains("private-answer"));
    }
    let records = recorded(&traces, 2).await;
    let encoded = records.to_string();
    for secret in [
        "private-prompt",
        "private-answer",
        "private-thought",
        "private-provider-key",
    ] {
        assert!(!encoded.contains(secret));
    }
    for trace in records["data"].as_array().unwrap() {
        assert_eq!(trace["outcome"], "completed");
        assert_eq!(trace["protocol"], "chat");
        assert_eq!(trace["model"], "opencode-go/deepseek-v4.1-flash");
        let stages = trace["stages"].as_array().unwrap();
        assert!(stages.iter().any(|s| s["stage"] == "client_request"));
        assert!(stages.iter().any(|s| s["stage"] == "upstream_request"));
        assert!(
            stages
                .iter()
                .any(|s| s["stage"] == "upstream_response" || s["stage"] == "upstream_event")
        );
        assert!(
            stages
                .iter()
                .any(|s| s["stage"] == "client_response" || s["stage"] == "client_event")
        );
    }
    let exported: Value = reqwest::get(format!("{admin}/api/traces"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(exported["data"].as_array().unwrap().len(), 2);
    task.abort();
    upstream_task.abort();
    admin_task.abort();
}

#[tokio::test]
async fn full_capture_requires_unexpired_consent_redacts_credentials_and_bounds_events() {
    let directory = tempfile::tempdir().unwrap();
    let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
    let policy = claude_proxy_engine::diagnostics::TracePolicy {
        enabled: true,
        full_until: Some(now() + 60),
        retention_hours: 1,
    };
    let capture = traces.start(&policy).unwrap();
    for _ in 0..600 {
        capture.stage(
            "client_request",
            &json!({"text":"consented-content","api_key":"never-store-key"}),
        );
    }
    capture.finish("completed");
    drop(capture);
    let records = recorded(&traces, 1).await;
    assert!(records.to_string().contains("consented-content"));
    assert!(!records.to_string().contains("never-store-key"));
    assert!(records["data"][0]["dropped_events"].as_u64().unwrap() > 0);
    traces.clear().unwrap();
    let expired = claude_proxy_engine::diagnostics::TracePolicy {
        full_until: Some(now() - 1),
        ..policy
    };
    let capture = traces.start(&expired).unwrap();
    capture.stage("client_request", &json!({"text":"expired-content"}));
    drop(capture);
    assert!(
        !recorded(&traces, 1)
            .await
            .to_string()
            .contains("expired-content")
    );
    traces.clear().unwrap();
    let policy = claude_proxy_engine::diagnostics::TracePolicy {
        enabled: true,
        full_until: None,
        retention_hours: 0,
    };
    let capture = traces.start(&policy).unwrap();
    drop(capture);
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(traces.list(None).unwrap()["data"], json!([]));
}

#[test]
#[ignore = "Requires disposable PostgreSQL and MySQL databases via PROXY_TEST_POSTGRES and PROXY_TEST_MYSQL"]
fn remote_database_roundtrips_settings_credentials_and_large_documents() {
    let databases = [
        Database::Postgres {
            url: std::env::var("PROXY_TEST_POSTGRES").expect("PROXY_TEST_POSTGRES"),
        },
        Database::Mysql {
            url: std::env::var("PROXY_TEST_MYSQL").expect("PROXY_TEST_MYSQL"),
        },
    ];
    for database in databases {
        let store = database.open().unwrap();
        let mut settings = store.load().unwrap();
        settings.host_id = "large-document".repeat(10_000);
        settings.providers = serde_json::from_value(json!([{"id":"test","name":"test","kind":"openai_compatible","base_url":"http://127.0.0.1:9999","api_key":"test-only-credential","enabled":false,"models":[]}])).unwrap();
        store.save(&settings).unwrap();
        assert_eq!(
            store.load().unwrap().providers[0].api_key.as_deref(),
            Some("test-only-credential")
        );
        drop(store);
        assert_eq!(
            database.open().unwrap().load().unwrap().host_id,
            settings.host_id
        );
    }
}
