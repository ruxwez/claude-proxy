mod http {
    use crate::provider::Protocol;
    use serde_json::{Value, json};
    fn request(content: Value) -> Value {
        json!({"model":"test/raw","max_tokens":128,"messages":[{"role":"user","content":content}]})
    }
    fn reconstruct(events: &[Value]) -> Value {
        let mut content: Vec<Value> = Vec::new();
        let mut arguments = std::collections::HashMap::<usize, String>::new();
        let mut result = json!({});
        for event in events {
            let index = event["index"].as_u64().unwrap_or(0) as usize;
            match event["type"].as_str().unwrap() {
                "content_block_start" => {
                    assert_eq!(index, content.len());
                    content.push(event["content_block"].clone());
                }
                "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                    let text = format!(
                        "{}{}",
                        content[index]["text"].as_str().unwrap(),
                        event["delta"]["text"].as_str().unwrap()
                    );
                    content[index]["text"] = json!(text);
                }
                "content_block_delta" => arguments
                    .entry(index)
                    .or_default()
                    .push_str(event["delta"]["partial_json"].as_str().unwrap()),
                "content_block_stop" => {
                    if let Some(args) = arguments.remove(&index) {
                        content[index]["input"] = serde_json::from_str(&args).unwrap();
                    }
                }
                "message_delta" => {
                    result["stop_reason"] = event["delta"]["stop_reason"].clone();
                    result["usage"] = event["usage"].clone();
                }
                _ => {}
            }
        }
        result["content"] = json!(content);
        result
    }

    use axum::{
        Json, Router,
        extract::{OriginalUri, State},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
    };
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::Duration,
    };
    type Calls = Arc<Mutex<Vec<(String, HeaderMap, Value)>>>;

    const UNIFIED_HEADERS: &[(&str, &str)] = &[
        ("anthropic-ratelimit-unified-status", "allowed_warning"),
        ("anthropic-ratelimit-unified-5h-utilization", "0.76"),
        ("anthropic-ratelimit-unified-7d-utilization", "0.96"),
        ("anthropic-ratelimit-unified-5h-reset", "1791800000"),
        (
            "anthropic-ratelimit-unified-representative-claim",
            "five_hour",
        ),
        ("anthropic-ratelimit-unified-overage-status", "allowed"),
        ("anthropic-ratelimit-unified-future-field", "unchanged"),
    ];

    fn assert_unified_headers(headers: &HeaderMap) {
        for &(name, value) in UNIFIED_HEADERS {
            assert_eq!(headers.get(name).unwrap(), value, "lost header {name}");
        }
        assert!(!headers.contains_key("set-cookie"));
    }

    async fn upstream(
        state: State<Calls>,
        uri: OriginalUri,
        headers: HeaderMap,
        body: Json<Value>,
    ) -> Response {
        let mut response = upstream_inner(state, uri, headers, body).await;
        for &(name, value) in UNIFIED_HEADERS {
            response.headers_mut().insert(name, value.parse().unwrap());
        }
        response
            .headers_mut()
            .insert("set-cookie", "private-cookie".parse().unwrap());
        response
    }

    async fn upstream_inner(
        State(calls): State<Calls>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        calls
            .lock()
            .unwrap()
            .push((uri.path().into(), headers, body.clone()));
        if body["model"] == "fail" {
            return (StatusCode::TOO_MANY_REQUESTS,[("request-id","provider-failure"),("retry-after","9")],Json(json!({"type":"error","error":{"type":"rate_limit_error","message":"limit","extra":"preserve"}}))).into_response();
        }
        if uri.path().ends_with("tokens") {
            if body["model"] == "missing" {
                return StatusCode::NOT_FOUND.into_response();
            }
            return Json(json!({"input_tokens":42})).into_response();
        }
        if body["stream"] == true {
            let data = if uri.path() == "/messages" {
                "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"native failure\",\"extra\":\"preserve\"}}\n\n".to_string()
            } else {
                [json!({"type":"response.output_text.delta","delta":"hello 🛑"}),json!({"type":"response.output_text.delta","delta":"ENDrest"}),json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hello 🛑ENDrest"}]}],"usage":{"input_tokens":20,"output_tokens":8}}})].iter().map(|e|format!("data: {e}\n\n")).collect()
            };
            return (
                [
                    ("content-type", "text/event-stream"),
                    ("request-id", "provider-stream"),
                ],
                data,
            )
                .into_response();
        }
        let result = if uri.path() == "/messages" {
            json!({"id":"msg_native","type":"message","role":"assistant","model":body["model"],"content":[{"type":"text","text":"native","extra":"preserve"}],"usage":{"input_tokens":7,"output_tokens":3,"cache_read_input_tokens":2}})
        } else {
            json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hello 🛑ENDrest"}]}],"usage":{"input_tokens":20,"output_tokens":8}})
        };
        (
            [
                ("request-id", "provider-ok"),
                ("x-ratelimit-remaining-requests", "19"),
                ("set-cookie", "never-forward"),
            ],
            Json(result),
        )
            .into_response()
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        (
            base,
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            }),
        )
    }
    async fn fixture(protocol: Protocol) -> (String, Calls, Vec<tokio::task::JoinHandle<()>>) {
        fixture_with_subscription(protocol, false).await
    }

    async fn fixture_with_subscription(
        protocol: Protocol,
        subscription: bool,
    ) -> (String, Calls, Vec<tokio::task::JoinHandle<()>>) {
        let calls = Calls::default();
        let (up, up_task) = serve(
            Router::new()
                .route("/messages", post(upstream))
                .route("/responses", post(upstream))
                .route("/messages/count_tokens", post(upstream))
                .route("/responses/input_tokens", post(upstream))
                .with_state(calls.clone()),
        )
        .await;
        let config = crate::Config {
            catalog: None,
            port: 1,
            timeout: Duration::from_secs(5),
            providers: vec![crate::ProviderConfig {
                zed: None,
                opencode: false,
                name: "test".into(),
                base: up,
                key: "provider-key".into(),
                free_only: false,
                models: vec![],
            }],
            client_version: "test".into(),
            project_id: "test".into(),
            protocols: HashMap::from([
                ("raw".into(), protocol),
                ("fail".into(), protocol),
                ("missing".into(), protocol),
                ("gpt-6.1-sol".into(), protocol),
                ("gpt-6-astra".into(), protocol),
                ("gpt-6-luna".into(), protocol),
            ]),
        };
        let subscriptions = if subscription {
            std::collections::HashSet::from(["test".to_string()])
        } else {
            std::collections::HashSet::new()
        };
        let (base, task) =
            serve(crate::server::router_with_subscriptions(config, subscriptions).unwrap()).await;
        (base, calls, vec![up_task, task])
    }

    #[tokio::test]
    async fn subscription_cache_markers_use_defaults_without_changing_content_or_schemas() {
        for subscription in [false, true] {
            let (base, calls, tasks) =
                fixture_with_subscription(Protocol::Responses, subscription).await;
            for model in ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-luna"] {
                for stream in [false, true] {
                    let mut req = request(
                        json!([{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]),
                    );
                    req["model"] = json!(format!("test/{model}"));
                    req["stream"] = json!(stream);
                    req["system"] = json!([{"type":"text","text":"Be concise.","cache_control":{"type":"ephemeral","ttl":"1h"}}]);
                    req["tools"] = json!([{"name":"lookup","input_schema":{"type":"object","properties":{"prompt_cache_breakpoint":{"type":"string"}}},"cache_control":{"type":"ephemeral"}}]);
                    let response = reqwest::Client::new()
                        .post(format!("{base}/v1/messages"))
                        .json(&req)
                        .send()
                        .await
                        .unwrap();
                    assert!(response.status().is_success());
                    assert_eq!(
                        response.headers()["x-claude-proxy-warnings"]
                            .to_str()
                            .unwrap()
                            .contains("prompt_cache_uses_provider_defaults"),
                        subscription
                    );
                    response.bytes().await.unwrap();
                    let body = calls.lock().unwrap().last().unwrap().2.clone();
                    assert_eq!(body.get("prompt_cache_options").is_none(), subscription);
                    assert_eq!(
                        body["input"][0]["content"][0]
                            .get("prompt_cache_breakpoint")
                            .is_none(),
                        subscription
                    );
                    assert_eq!(
                        body["input"][1]["content"][0]
                            .get("prompt_cache_breakpoint")
                            .is_none(),
                        subscription
                    );
                    assert_eq!(body["input"][0]["content"][0]["text"], "Be concise.");
                    assert_eq!(body["input"][1]["content"][0]["text"], "hello");
                    assert_eq!(
                        body["tools"][0]["parameters"],
                        req["tools"][0]["input_schema"]
                    );
                }
                let req = json!({"model":format!("test/{model}"),"messages":[{"role":"user","content":[{"type":"text","text":"count","cache_control":{"type":"ephemeral"}}]}]});
                let response = reqwest::Client::new()
                    .post(format!("{base}/v1/messages/count_tokens"))
                    .json(&req)
                    .send()
                    .await
                    .unwrap();
                assert!(response.status().is_success());
                let body = calls.lock().unwrap().last().unwrap().2.clone();
                assert_eq!(
                    body["input"][0]["content"][0]
                        .get("prompt_cache_breakpoint")
                        .is_none(),
                    subscription
                );
            }
            for task in tasks {
                task.abort();
            }
        }
    }

    #[tokio::test]
    async fn subscription_omits_safety_identifier_but_api_key_responses_keeps_it() {
        for subscription in [false, true] {
            let (base, calls, tasks) =
                fixture_with_subscription(Protocol::Responses, subscription).await;
            for stream in [false, true] {
                let mut req = request(json!("hi"));
                req["stream"] = json!(stream);
                req["metadata"] = json!({"user_id":"private-user@example.com"});
                req["tools"] =
                    json!([{"name":"lookup","input_schema":{"type":"object","properties":{}}}]);
                let response = reqwest::Client::new()
                    .post(format!("{base}/v1/messages"))
                    .json(&req)
                    .send()
                    .await
                    .unwrap();
                assert!(response.status().is_success());
                let warning = response.headers()["x-claude-proxy-warnings"]
                    .to_str()
                    .unwrap();
                assert_eq!(
                    warning.contains("user_id_not_supported_by_subscription"),
                    subscription
                );
                assert_eq!(warning.contains("user_id_forwarded_as_hash"), !subscription);
                response.bytes().await.unwrap();
                let calls = calls.lock().unwrap();
                let body = &calls.last().unwrap().2;
                let mut expected =
                    crate::translate::request(&req, "raw", Protocol::Responses).unwrap();
                if subscription {
                    expected
                        .as_object_mut()
                        .unwrap()
                        .remove("safety_identifier");
                }
                assert_eq!(*body, expected);
                if !subscription {
                    assert_eq!(body["safety_identifier"].as_str().unwrap().len(), 64);
                    assert_ne!(body["safety_identifier"], req["metadata"]["user_id"]);
                }
            }
            for task in tasks {
                task.abort();
            }
        }
    }

    #[tokio::test]
    async fn native_body_beta_headers_errors_and_stream_errors_pass_through() {
        let (base, calls, tasks) = fixture(Protocol::Messages).await;
        let client = reqwest::Client::new();
        let req = request(
            json!([{"type":"document","source":{"type":"file","file_id":"anthropic-file"}}]),
        );
        let response = client
            .post(format!("{base}/v1/messages"))
            .header("anthropic-beta", "test-beta")
            .header("x-api-key", "client-key")
            .json(&req)
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()["request-id"], "provider-ok");
        assert_unified_headers(response.headers());
        assert!(response.headers().get("set-cookie").is_none());
        let raw: Value = response.json().await.unwrap();
        assert_eq!(raw["id"], "msg_native");
        assert_eq!(raw["content"][0]["extra"], "preserve");
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls[0].1["anthropic-beta"], "test-beta");
            assert_eq!(calls[0].1["x-api-key"], "provider-key");
            let mut expected = req.clone();
            expected["model"] = json!("raw");
            assert_eq!(calls[0].2, expected);
        }
        let mut fail = request(json!("hi"));
        fail["model"] = json!("test/fail");
        let response = client
            .post(format!("{base}/v1/messages"))
            .json(&fail)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "9");
        assert_eq!(response.headers()["request-id"], "provider-failure");
        assert_unified_headers(response.headers());
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["extra"],
            "preserve"
        );
        let mut req = request(json!("hi"));
        req["stream"] = json!(true);
        let response = client
            .post(format!("{base}/v1/messages"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert_unified_headers(response.headers());
        let body = response.text().await.unwrap();
        assert!(body.contains("overloaded_error") && body.contains("preserve"));
        assert!(!body.contains("message_stop"));
        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn stop_sequences_and_transport_headers_work_in_json_and_sse() {
        let (base, _, tasks) = fixture(Protocol::Responses).await;
        for stream in [false, true] {
            let mut req = request(json!("hi"));
            req["stream"] = json!(stream);
            req["stop_sequences"] = json!(["🛑END"]);
            let response = reqwest::Client::new()
                .post(format!("{base}/v1/messages"))
                .json(&req)
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            assert!(
                response.headers()["x-claude-proxy-warnings"]
                    .to_str()
                    .unwrap()
                    .contains("stop_sequences_applied_by_proxy")
            );
            assert!(response.headers().contains_key("request-id"));
            assert_unified_headers(response.headers());
            let result = if stream {
                let body = response.bytes().await.unwrap();
                let mut decoder = crate::stream::Decoder::default();
                let events: Vec<Value> = decoder
                    .push(&body, true)
                    .unwrap()
                    .iter()
                    .map(|s| serde_json::from_str(s).unwrap())
                    .collect();
                assert_eq!(events[events.len() - 2]["delta"]["stop_sequence"], "🛑END");
                reconstruct(&events)
            } else {
                response.json::<Value>().await.unwrap()
            };
            assert_eq!(result["stop_reason"], "stop_sequence");
            assert_eq!(result["content"][0]["text"], "hello ");
        }
        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn token_counts_use_provider_and_estimate_only_when_endpoint_is_missing() {
        for protocol in [Protocol::Messages, Protocol::Responses] {
            let (base, calls, tasks) = fixture(protocol).await;
            for model in ["raw", "missing", "fail"] {
                let mut req = request(json!("hi"));
                req["model"] = json!(format!("test/{model}"));
                let response = reqwest::Client::new()
                    .post(format!("{base}/v1/messages/count_tokens"))
                    .json(&req)
                    .send()
                    .await
                    .unwrap();
                assert_unified_headers(response.headers());
                if model == "fail" {
                    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                    assert_eq!(response.headers()["request-id"], "provider-failure");
                    assert_eq!(response.headers()["retry-after"], "9");
                    let error = response.json::<Value>().await.unwrap();
                    assert_eq!(error["error"]["type"], "rate_limit_error");
                    if protocol == Protocol::Messages {
                        assert_eq!(error["error"]["extra"], "preserve");
                    }
                } else {
                    assert_eq!(
                        response.headers()["x-claude-proxy-token-count"],
                        if model == "raw" {
                            "provider"
                        } else {
                            "estimated"
                        }
                    );
                    let count = response.json::<Value>().await.unwrap()["input_tokens"]
                        .as_u64()
                        .unwrap();
                    if model == "raw" {
                        assert_eq!(count, 42);
                    } else {
                        assert!(count > 0);
                    }
                }
            }
            assert!(calls.lock().unwrap().iter().all(
                |(_, _, body)| body.get("max_tokens").is_none() && body.get("stream").is_none()
            ));
            for task in tasks {
                task.abort();
            }
        }
    }

    #[tokio::test]
    async fn chat_token_estimate_does_not_invent_subscription_quota_or_call_inference() {
        let (base, calls, tasks) = fixture(Protocol::Chat).await;
        let response = reqwest::Client::new()
            .post(format!("{base}/v1/messages/count_tokens"))
            .json(&json!({"model":"test/raw","messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(
            response.headers()["x-claude-proxy-token-count"],
            "estimated"
        );
        assert!(
            !response
                .headers()
                .keys()
                .any(|name| name.as_str().starts_with("anthropic-ratelimit-unified-"))
        );
        assert!(calls.lock().unwrap().is_empty());
        for task in tasks {
            task.abort();
        }
    }
}
