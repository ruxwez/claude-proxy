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
async fn debug_captures_all_boundaries_and_deepseek_sse_with_redacted_credentials() {
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
    let settings: Settings = serde_json::from_value(json!({"providers":[{"id":"opencode-go","name":"Go","kind":"opencode_go","base_url":upstream,"api_key":"private-provider-key","models":[{"id":"deepseek-v4.1-flash","protocol":"chat"}]}],"traces":{"debug":true}})).unwrap();
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
    assert!(!encoded.contains("private-provider-key"));
    for content in ["private-prompt", "private-answer", "private-thought"] {
        assert!(encoded.contains(content));
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
async fn debug_preserves_large_payloads_all_events_and_json_pointer_changes() {
    use claude_proxy_engine::diagnostics::{TRACE_RETENTION_SECONDS, TracePolicy};
    let directory = tempfile::tempdir().unwrap();
    let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
    let capture = traces.start(&TracePolicy { debug: true }).unwrap();
    capture.stage("client_request", &json!({"text":"x".repeat(160*1024),"api_key":"never-store-key", "output_config":{"effort":"xhigh"}}));
    capture.stage(
        "upstream_request",
        &json!({"reasoning_effort":"high","api_key":"new-secret"}),
    );
    for index in 0..600 {
        capture.stage("upstream_event", &json!({"index":index,"text":"chunk"}));
    }
    capture.finish("completed");
    drop(capture);
    traces.flush().unwrap();
    let records = traces.list(None).unwrap();
    let record = &records["data"][0];
    assert_eq!(
        record["expires"].as_u64().unwrap() - record["created"].as_u64().unwrap(),
        TRACE_RETENTION_SECONDS
    );
    assert_eq!(record["stages"].as_array().unwrap().len(), 602);
    assert_eq!(record["dropped_events"], 0);
    assert_eq!(
        record["stages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .len(),
        160 * 1024
    );
    assert!(!records.to_string().contains("never-store-key"));
    assert!(!records.to_string().contains("new-secret"));
    let changes = record["stages"][1]["changes"].as_array().unwrap();
    assert!(
        changes
            .iter()
            .any(|change| change["path"] == "/output_config" && change["change"] == "removed")
    );
    assert!(
        changes
            .iter()
            .any(|change| change["path"] == "/reasoning_effort" && change["after"] == "high")
    );
    assert!(!changes.iter().any(|change| change["path"] == "/api_key"));
}

#[tokio::test]
async fn normal_mode_persists_only_errors_even_with_legacy_disabled_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
    let policy =
        serde_json::from_value(json!({"enabled":false,"retention_hours":0,"full_until":now()+60}))
            .unwrap();
    for outcome in [
        "completed",
        "invalid_request",
        "upstream_error",
        "transport_error",
        "conversion_error",
        "stream_error",
        "cancelled",
    ] {
        let capture = traces.start(&policy).unwrap();
        capture.stage("client_request", &json!({"text":"private-prompt"}));
        capture.finish(outcome);
        drop(capture);
    }
    let capture = traces.start(&policy).unwrap();
    capture.comparison(&json!({"confirmed_losses":["response_text_lost"]}));
    capture.finish("completed");
    drop(capture);
    traces.flush().unwrap();
    let records = traces.list(None).unwrap();
    assert_eq!(records["data"].as_array().unwrap().len(), 7);
    assert!(!records.to_string().contains("private-prompt"));
    assert!(
        records["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["outcome"] != "completed")
    );
}

#[tokio::test]
async fn errors_record_provider_input_and_actual_client_output_in_both_modes() {
    use axum::response::IntoResponse;
    let (upstream, upstream_task) = serve(Router::new().route(
        "/chat/completions",
        post(|Json(body): Json<Value>| async move {
            match body["messages"][0]["content"].as_str().unwrap_or_default() {
                "reject" => (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({"error":{"message":"rate limit"}})),
                )
                    .into_response(),
                "invalid" => "not-json".into_response(),
                "truncated" => (
                    [("content-type", "text/event-stream")],
                    concat!(
                        r#"data: {"choices":[{"delta":{"content":"partial"}}]}"#,
                        "\n\n"
                    ),
                )
                    .into_response(),
                _ => Json(json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}))
                    .into_response(),
            }
        }),
    ))
    .await;
    for debug in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
        let settings = serde_json::from_value(json!({"providers":[{"id":"test","name":"test","kind":"openai_compatible","base_url":upstream,"api_key":"test-key","models":[{"id":"deepseek-v4.1-flash","protocol":"chat"}]}],"traces":{"debug":debug}})).unwrap();
        let router =
            crate::server::configured_router_with_traces(&settings, Some(traces.clone())).unwrap();
        let (endpoint, task) = serve(router).await;
        let http = reqwest::Client::new();
        for (content, stream, outcome) in [
            ("ok", false, "completed"),
            ("reject", false, "upstream_error"),
            ("invalid", false, "conversion_error"),
            ("truncated", true, "stream_error"),
        ] {
            let response = http.post(format!("{endpoint}/v1/messages"))
                .json(&json!({"model":"test/deepseek-v4.1-flash","max_tokens":64,"stream":stream,"messages":[{"role":"user","content":content}]})).send().await.unwrap();
            let id = response.headers()["x-claude-proxy-trace-id"]
                .to_str()
                .unwrap()
                .to_owned();
            let sent: String = response.text().await.unwrap();
            traces.flush().unwrap();
            let recorded = traces.list(Some(&id)).unwrap();
            if !debug && outcome == "completed" {
                assert_eq!(recorded["data"], json!([]));
                continue;
            }
            let trace = &recorded["data"][0];
            assert_eq!(trace["outcome"], outcome);
            assert_eq!(trace["stages"][0]["stage"], "client_request");
            assert_eq!(trace["stages"][1]["stage"], "upstream_request");
            let stages = trace["stages"].as_array().unwrap();
            assert!(
                stages
                    .iter()
                    .any(|stage| stage["boundary"] == "provider_to_proxy")
            );
            assert!(
                stages
                    .iter()
                    .any(|stage| stage["boundary"] == "proxy_to_claude")
            );
            if debug || outcome != "completed" {
                let output = stages.last().unwrap()["content"].clone();
                if stream {
                    assert_eq!(output["type"], "error");
                    assert!(sent.contains(output["error"]["message"].as_str().unwrap()));
                } else {
                    assert_eq!(output, serde_json::from_str::<Value>(&sent).unwrap());
                }
                if debug && content == "invalid" {
                    assert!(stages.iter().any(|stage| stage["content"] == "not-json"));
                }
            }
        }
        // Even malformed JSON and failures before routing have a client error boundary.
        let response = http
            .post(format!("{endpoint}/v1/messages"))
            .header("content-type", "application/json")
            .body("{broken")
            .send()
            .await
            .unwrap();
        let id = response.headers()["x-claude-proxy-trace-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let sent: Value = response.json().await.unwrap();
        traces.flush().unwrap();
        let records = traces.list(Some(&id)).unwrap();
        assert_eq!(records["data"][0]["outcome"], "invalid_request");
        if debug {
            assert_eq!(records["data"][0]["stages"][1]["content"], sent);
        }
        task.abort();
    }
    upstream_task.abort();
}

#[tokio::test]
async fn token_count_traces_capture_native_boundaries_and_authentication_failures() {
    use axum::response::IntoResponse;
    let (upstream, upstream_task) = serve(Router::new().route(
        "/messages/count_tokens",
        post(|| async {
            Json(json!({"input_tokens":7,"provider_detail":"counted"})).into_response()
        }),
    ))
    .await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(&directory.path().join("settings.db")).unwrap());
    let settings = serde_json::from_value(json!({"providers":[{"id":"test","name":"test","kind":"openai_compatible","base_url":upstream,"api_key":"private-provider-key","models":[{"id":"claude-test","protocol":"messages"}]}],"traces":{"debug":true}})).unwrap();
    store.save(&settings).unwrap();
    let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
    let control = Arc::new(Control {
        auth: crate::auth::Auth::new(Some("shared-test-key")),
        application: Arc::new(Application::open(store, 10800).unwrap()),
        traces: Some(traces.clone()),
        bootstrap: None,
        allowed_hosts: vec![],
        configured: AtomicBool::new(true),
    });
    let (endpoint, task) = serve(management::gateway_with_control(control)).await;
    let http = reqwest::Client::new();
    let req = json!({"model":"test/claude-test","messages":[{"role":"user","content":"hi"}]});
    for (key, expected) in [("shared-test-key", 200), ("wrong", 401)] {
        let response = http
            .post(format!("{endpoint}/v1/messages/count_tokens"))
            .header("x-api-key", key)
            .json(&req)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        let id = response.headers()["x-claude-proxy-trace-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let sent: Value = response.json().await.unwrap();
        traces.flush().unwrap();
        let record = traces.list(Some(&id)).unwrap();
        let trace = &record["data"][0];
        assert_eq!(trace["operation"], "count_tokens");
        if expected == 200 {
            assert_eq!(trace["stages"][0]["content"], req);
        } else {
            assert_eq!(
                trace["stages"][0]["content_omitted"],
                "authentication_rejected_before_body"
            );
        }
        let stages = trace["stages"].as_array().unwrap();
        assert_eq!(stages.last().unwrap()["content"], sent);
        assert_eq!(stages.len(), if expected == 200 { 4 } else { 2 });
        assert_eq!(
            trace["outcome"],
            if expected == 200 {
                "completed"
            } else {
                "authentication_error"
            }
        );
    }
    task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn a_stream_dropped_before_polling_is_recorded_as_cancelled() {
    use axum::response::IntoResponse;
    let (endpoint, task) = serve(Router::new().route(
        "/stream",
        axum::routing::get(|| async {
            ([("content-type", "text/event-stream")], "data: [DONE]\n\n").into_response()
        }),
    ))
    .await;
    let upstream = reqwest::get(format!("{endpoint}/stream")).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
    let capture = traces.start(&Default::default()).unwrap();
    capture.finish("transport_error");
    let response = crate::stream::response_traced(
        upstream,
        crate::domain::Protocol::Chat,
        "test".into(),
        &json!({}),
        Some(capture.clone()),
    );
    drop(response);
    drop(capture);
    traces.flush().unwrap();
    assert_eq!(
        traces.list(None).unwrap()["data"][0]["outcome"],
        "cancelled"
    );
    task.abort();
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
