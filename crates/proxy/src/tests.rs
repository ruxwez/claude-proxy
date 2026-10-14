mod http {
    use crate::{
        Config,
        provider::{Protocol, selected_protocol, valid_identifier},
        server::router,
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
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[derive(Default)]
    struct Mock {
        calls: Mutex<Vec<(String, HeaderMap, Value)>>,
        model_calls: AtomicUsize,
    }

    async fn upstream_models(State(mock): State<Arc<Mock>>) -> Json<Value> {
        mock.model_calls.fetch_add(1, Ordering::Relaxed);
        Json(
            json!({"data":[{"id":"deepseek-v4.1-flash"},{"id":"muse-spark-1.3-contributor"},{"id":"claude-haiku-5-5"},{"id":"sample-free"}]}),
        )
    }

    async fn upstream(
        State(mock): State<Arc<Mock>>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(v): Json<Value>,
    ) -> Response {
        mock.calls
            .lock()
            .unwrap()
            .push((uri.path().into(), headers, v.clone()));
        if v["model"] == "fail" {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "7")],
                Json(json!({"error":{"message":"rate limited"}})),
            )
                .into_response();
        }
        if v["stream"] == true {
            let s = if uri.path().ends_with("/responses") {
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n"
            } else if uri.path().ends_with("/messages") {
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            } else {
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n"
            };
            return ([("content-type", "text/event-stream")], s).into_response();
        }
        Json(if uri.path().ends_with("/responses") {json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"OK"}]}],"usage":{"input_tokens":4,"output_tokens":2}})}
        else if uri.path().ends_with("/messages") {json!({"type":"message","content":[{"type":"text","text":"OK"}],"model":v["model"],"stop_reason":"end_turn","usage":{"input_tokens":4,"output_tokens":2}})}
        else {json!({"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":2}})}).into_response()
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, task)
    }

    async fn tool_workflow(
        State(mock): State<Arc<Mock>>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        mock.calls
            .lock()
            .unwrap()
            .push((uri.path().into(), headers, body.clone()));
        let responses = uri.path() == "/responses";
        let history = body[if responses { "input" } else { "messages" }]
            .as_array()
            .unwrap();
        let has_results = history
            .iter()
            .any(|m| m["role"] == "tool" || m["type"] == "function_call_output");
        let mut raw = if responses {
            json!({"status":"completed","output":[],"usage":{"input_tokens":9,"output_tokens":5}})
        } else {
            json!({"choices":[{"index":0,"message":{"content":null},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":5}})
        };
        if responses {
            raw["output"] = if has_results {
                json!([{"type":"message","content":[{"type":"output_text","text":"finished"}]}])
            } else {
                json!([{"type":"reasoning","id":"rs","encrypted_content":"opaque","summary":[]},
                    {"type":"function_call","id":"i1","call_id":"c1","name":"read","arguments":"{\"path\":\"a\"}"},
                    {"type":"function_call","id":"i2","call_id":"c2","name":"read","arguments":"{\"path\":\"b\"}"}])
            };
        } else if has_results {
            raw["choices"][0]["message"]["content"] = json!("finished");
        } else {
            raw["choices"][0]["finish_reason"] = json!("tool_calls");
            raw["choices"][0]["message"]["tool_calls"] = json!([
                {"id":"c1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}},
                {"id":"c2","type":"function","function":{"name":"read","arguments":"{\"path\":\"b\"}"}}]);
        }
        if body["stream"] != true {
            return Json(raw).into_response();
        }
        let frames = if responses {
            let mut frames = Vec::new();
            for (index, item) in raw["output"].as_array().unwrap().iter().enumerate() {
                if item["type"] == "message" {
                    frames.push(json!({"type":"response.output_text.delta","output_index":index,"delta":"finished"}));
                } else {
                    frames.push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
                }
            }
            frames.push(json!({"type":"response.completed","response":raw}));
            frames
        } else {
            let mut delta = raw["choices"][0]["message"].clone();
            if let Some(calls) = delta["tool_calls"].as_array_mut() {
                for (index, call) in calls.iter_mut().enumerate() {
                    call["index"] = json!(index);
                }
            }
            vec![
                json!({"choices":[{"index":0,"delta":delta,"finish_reason":raw["choices"][0]["finish_reason"]}],"usage":raw["usage"]}),
            ]
        };
        let mut wire = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect::<String>();
        if !responses {
            wire.push_str("data: [DONE]\n\n");
        }
        ([("content-type", "text/event-stream")], wire).into_response()
    }

    #[tokio::test]
    async fn http_parallel_tools_round_trip_with_reasoning_streaming_and_json() {
        use crate::stream::Decoder;
        let mock = Arc::new(Mock::default());
        let (up, up_task) = serve(
            Router::new()
                .route("/chat/completions", post(tool_workflow))
                .route("/responses", post(tool_workflow))
                .with_state(mock.clone()),
        )
        .await;
        let mut config = Config::from_values(&HashMap::from([
            ("OPENAI_ENABLED".into(), "true".into()),
            ("OPENAI_BASE_URL".into(), up),
        ]))
        .unwrap();
        config
            .protocols
            .insert("gpt-6-luna".into(), Protocol::Responses);
        let (base, task) = serve(router(config).unwrap()).await;
        let client = reqwest::Client::new();
        for model in ["o3", "gpt-6-luna"] {
            for streaming in [false, true] {
                let mut req = json!({"model":format!("openai/{model}"),"messages":[{"role":"user","content":"read both"}],
                    "max_tokens":64,"stream":streaming,"thinking":{"type":"adaptive"},"output_config":{"effort":"high"},
                    "tools":[{"name":"read","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}],
                    "tool_choice":{"type":"auto","disable_parallel_tool_use":false}});
                for round in 0..2 {
                    let response = client
                        .post(format!("{base}/v1/messages"))
                        .json(&req)
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                    let (content, reason) = if streaming {
                        let wire = response.bytes().await.unwrap();
                        let frames = Decoder::default().push(&wire, true).unwrap();
                        let events: Vec<Value> = frames
                            .iter()
                            .map(|frame| serde_json::from_str(frame).unwrap())
                            .collect();
                        assert_eq!(events[0]["type"], "message_start");
                        assert_eq!(events.last().unwrap()["type"], "message_stop");
                        let content_events: Vec<_> = events
                            .iter()
                            .filter(|event| event["type"] != "message_start")
                            .cloned()
                            .collect();
                        (
                            json!(super::translation::reconstructed(&content_events)),
                            events[events.len() - 2]["delta"]["stop_reason"].clone(),
                        )
                    } else {
                        let value: Value = response.json().await.unwrap();
                        (value["content"].clone(), value["stop_reason"].clone())
                    };
                    if round == 0 {
                        assert_eq!(reason, "tool_use");
                        let tools: Vec<_> = content
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|b| b["type"] == "tool_use")
                            .collect();
                        assert_eq!(tools.len(), 2);
                        assert_eq!(tools[0]["id"], "c1");
                        assert_eq!(tools[1]["id"], "c2");
                        req["messages"].as_array_mut().unwrap().extend([
                            json!({"role":"assistant","content":content}),
                            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"A"},
                                {"type":"tool_result","tool_use_id":"c2","content":"B"}]})]);
                    } else {
                        assert_eq!(reason, "end_turn");
                        assert_eq!(content[0]["text"], "finished");
                        let calls = mock.calls.lock().unwrap();
                        let (_, _, body) = calls.last().unwrap();
                        assert_eq!(body["parallel_tool_calls"], true);
                        if model == "o3" {
                            assert_eq!(body["reasoning_effort"], "high");
                            assert_eq!(body["messages"][2]["tool_call_id"], "c1");
                            assert_eq!(body["messages"][3]["tool_call_id"], "c2");
                        } else {
                            assert_eq!(body["reasoning"]["effort"], "high");
                            let input = body["input"].as_array().unwrap();
                            assert!(
                                input.iter().any(|b| b["type"] == "reasoning"
                                    && b["encrypted_content"] == "opaque")
                            );
                            let results: Vec<_> = input
                                .iter()
                                .filter(|b| b["type"] == "function_call_output")
                                .collect();
                            assert_eq!(results[0]["call_id"], "c1");
                            assert_eq!(results[1]["call_id"], "c2");
                        }
                    }
                }
            }
        }
        task.abort();
        up_task.abort();
    }

    async fn delayed_upstream(OriginalUri(uri): OriginalUri, Json(v): Json<Value>) -> Response {
        use axum::body::{Body, Bytes};
        use std::{convert::Infallible, time::Duration};
        let model = v["model"].as_str().unwrap().to_string();
        if model == "headers" {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let (delta, done) = if uri.path().ends_with("/messages") {
            (
                "data: {\"type\":\"ping\"}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n",
            )
        } else if uri.path().ends_with("/responses") {
            (
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"a\"}\n\n",
                "data: {\"type\":\"response.completed\"}\n\n",
            )
        } else {
            (
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
        };
        let source = async_stream::stream! {
            if model == "keepalive" {
                // A ping must not corrupt an upstream event split across reads.
                yield Ok::<_, Infallible>(Bytes::from_static(&delta.as_bytes()[..10]));
                tokio::time::sleep(Duration::from_secs(16)).await;
                yield Ok(Bytes::from_static(&delta.as_bytes()[10..]));
            } else {
                for _ in 0..6 {
                    yield Ok(Bytes::from_static(delta.as_bytes()));
                    if model == "stall" {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    } else {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
            }
            if model != "truncated" { yield Ok(Bytes::from_static(done.as_bytes())); }
        };
        (
            [("content-type", "text/event-stream")],
            Body::from_stream(source),
        )
            .into_response()
    }

    #[tokio::test]
    async fn native_stream_preserves_thinking_signatures_and_redactions() {
        use crate::stream::{self, Decoder};
        let expected = vec![
            json!({"type":"message_start","message":{"id":"native","role":"assistant","content":[]}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"native thought"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-native-signature"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque-redaction"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}),
            json!({"type":"message_stop"}),
        ];
        let wire = expected
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>();
        let (up, task) = serve(Router::new().route(
            "/messages",
            post(move || async move { ([("content-type", "text/event-stream")], wire) }),
        ))
        .await;
        let upstream = reqwest::Client::new()
            .post(format!("{up}/messages"))
            .send()
            .await
            .unwrap();
        let response = stream::response(upstream, Protocol::Messages, "raw".into(), &json!({}));
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let actual: Vec<Value> = Decoder::default()
            .push(&bytes, true)
            .unwrap()
            .iter()
            .map(|frame| serde_json::from_str(frame).unwrap())
            .collect();
        assert_eq!(actual, expected);
        task.abort();
    }

    #[tokio::test]
    async fn streaming_uses_idle_timeout_and_reports_stalls_and_truncation() {
        use std::time::Duration;
        let (up, up_task) = serve(
            Router::new()
                .route("/chat/completions", post(delayed_upstream))
                .route("/responses", post(delayed_upstream))
                .route("/messages", post(delayed_upstream)),
        )
        .await;
        for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Messages] {
            let mut config = Config::from_values(&HashMap::from([
                ("OPENCODE_GO_ENABLED".into(), "true".into()),
                ("OPENCODE_GO_BASE_URL".into(), up.clone()),
            ]))
            .unwrap();
            config.timeout = Duration::from_millis(500);
            for model in ["slow", "stall", "headers", "truncated"] {
                config.protocols.insert(model.into(), protocol);
            }
            let (base, task) = serve(router(config).unwrap()).await;
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            for model in ["slow", "stall", "truncated", "headers"] {
                let response = client.post(format!("{base}/v1/messages"))
                    .json(&json!({"model":format!("opencode-go/{model}"),"max_tokens":8,"stream":true,"messages":[{"role":"user","content":"hi"}]}))
                    .send().await.unwrap();
                if model == "headers" {
                    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
                } else {
                    assert_eq!(response.status(), StatusCode::OK);
                    let text = response.text().await.unwrap();
                    if model == "slow" {
                        assert!(text.contains("message_stop"), "{protocol:?}: {text}");
                        assert!(!text.contains("event: error"), "{protocol:?}: {text}");
                    } else {
                        assert!(text.contains("event: error"), "{protocol:?}: {text}");
                        assert!(!text.contains("message_stop"));
                    }
                }
            }
            task.abort();
        }
        up_task.abort();
    }

    #[tokio::test]
    async fn keepalive_preserves_split_events_in_all_protocols() {
        use crate::{
            provider::Protocol,
            stream::{self, Decoder},
        };
        use std::time::Duration;
        let (up, up_task) = serve(
            Router::new()
                .route("/chat/completions", post(delayed_upstream))
                .route("/responses", post(delayed_upstream))
                .route("/messages", post(delayed_upstream)),
        )
        .await;
        let client = reqwest::Client::new();
        futures_util::future::join_all(
            [
                (Protocol::Chat, "/chat/completions"),
                (Protocol::Responses, "/responses"),
                (Protocol::Messages, "/messages"),
            ]
            .into_iter()
            .map(|(protocol, path)| {
                let client = &client;
                let up = &up;
                async move {
                    let upstream = client
                        .post(format!("{up}{path}"))
                        .json(&json!({"model":"keepalive"}))
                        .send()
                        .await
                        .unwrap();
                    let response = stream::response(upstream, protocol, "test".into(), &json!({}));
                    use futures_util::StreamExt;
                    let mut source = response.into_body().into_data_stream();
                    let mut decoder = Decoder::default();
                    let events = tokio::time::timeout(Duration::from_secs(17), async {
                        loop {
                            let chunk = source.next().await.unwrap().unwrap();
                            let events = decoder.push(&chunk, false).unwrap();
                            if events.iter().any(|e| {
                                serde_json::from_str::<Value>(e).unwrap()["type"] == "ping"
                            }) {
                                break events;
                            }
                        }
                    })
                    .await
                    .unwrap();
                    assert!(events.iter().any(|e| e.contains("ping")));
                    let mut rest = Vec::new();
                    while let Some(chunk) = source.next().await {
                        rest.extend(decoder.push(&chunk.unwrap(), false).unwrap());
                    }
                    assert!(rest.iter().any(
                        |e| serde_json::from_str::<Value>(e).unwrap()["type"] == "message_stop"
                    ));
                    assert!(!rest.iter().any(|e| e.contains("api_error")));
                }
            }),
        )
        .await;
        up_task.abort();
    }

    #[tokio::test]
    async fn all_protocols_streaming_headers_errors_pagination_and_cache() {
        let mock = Arc::new(Mock::default());
        let (up, up_task) = serve(
            Router::new()
                .route("/v1/models", get(upstream_models))
                .route("/v1/chat/completions", post(upstream))
                .route("/v1/responses", post(upstream))
                .route("/v1/messages", post(upstream))
                .with_state(mock.clone()),
        )
        .await;
        let values = HashMap::from([
            ("OPENCODE_GO_ENABLED".into(), "true".into()),
            ("OPENCODE_API_KEY".into(), "provider-secret".into()),
            ("OPENCODE_GO_BASE_URL".into(), format!("{up}/v1")),
        ]);
        let (base, task) = serve(router(Config::from_values(&values).unwrap()).unwrap()).await;
        let client = reqwest::Client::new();
        for streaming in [false, true] {
            for (model, path) in [
                ("deepseek-v4.1-flash", "/v1/chat/completions"),
                ("muse-spark-1.3-contributor", "/v1/responses"),
                ("claude-haiku-5-5", "/v1/messages"),
            ] {
                let mut request = json!({"model":format!("opencode-go/{model}"),"messages":[{"role":"user","content":"hi"}],"max_tokens":8,"stream":streaming});
                if model == "claude-haiku-5-5" {
                    request["custom_native_field"] = json!("preserved");
                }
                let response = client
                    .post(format!("{base}/v1/messages"))
                    .header("x-api-key", "client-key")
                    .bearer_auth("client-token")
                    .header("x-claude-code-session-id", "stable-session")
                    .json(&request)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                if streaming {
                    let text = response.text().await.unwrap();
                    assert!(text.contains("message_stop"));
                    if model != "claude-haiku-5-5" {
                        assert!(text.contains("text_delta"));
                    }
                } else {
                    let v: Value = response.json().await.unwrap();
                    assert_eq!(v["content"][0]["text"], "OK");
                }
                let calls = mock.calls.lock().unwrap();
                let (actual, headers, body) = calls.last().unwrap();
                assert_eq!(actual, path);
                assert_eq!(body["model"], model);
                assert_eq!(headers["x-opencode-client"], "cli");
                assert!(valid_identifier(
                    headers["x-opencode-session"].to_str().unwrap(),
                    "ses"
                ));
                assert!(valid_identifier(
                    headers["x-opencode-request"].to_str().unwrap(),
                    "msg"
                ));
                if model == "claude-haiku-5-5" {
                    assert_eq!(headers["x-api-key"], "provider-secret");
                    assert!(headers.get("authorization").is_none());
                    assert_eq!(body["custom_native_field"], "preserved");
                } else {
                    assert_eq!(headers["authorization"], "Bearer provider-secret");
                    assert!(headers.get("x-api-key").is_none());
                }
            }
        }
        {
            let calls = mock.calls.lock().unwrap();
            let sessions: Vec<_> = calls
                .iter()
                .map(|c| c.1["x-opencode-session"].clone())
                .collect();
            assert!(sessions.iter().all(|s| s == sessions[0]));
        }
        let first: Value = client
            .get(format!("{base}/v1/models?limit=2"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(first["has_more"], true);
        let second: Value = client
            .get(format!("{base}/v1/models"))
            .query(&[("after_id", first["last_id"].as_str().unwrap())])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(second["data"].as_array().unwrap().len(), 2);
        assert_eq!(mock.model_calls.load(Ordering::Relaxed), 1);
        client
            .get(format!("{base}/v1/models?refresh=true"))
            .send()
            .await
            .unwrap();
        assert_eq!(mock.model_calls.load(Ordering::Relaxed), 2);
        let detail = client
            .get(format!("{base}/v1/models/opencode-go/deepseek-v4.1-flash"))
            .send()
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        for (model, expected) in [
            ("opencode-go/fail", StatusCode::TOO_MANY_REQUESTS),
            ("missing/x", StatusCode::NOT_FOUND),
            ("no-prefix", StatusCode::NOT_FOUND),
        ] {
            let r = client
            .post(format!("{base}/v1/messages"))
            .json(
                &json!({"model":model,"max_tokens":8,"messages":[{"role":"user","content":"hi"}]}),
            )
            .send()
            .await
            .unwrap();
            assert_eq!(r.status(), expected);
            if model == "opencode-go/fail" {
                assert_eq!(r.headers()["retry-after"], "7");
            }
        }
        let count: Value = client
            .post(format!("{base}/v1/messages/count_tokens"))
            .json(&json!({"messages":[{"role":"user","content":"hello"}]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(count["input_tokens"].as_u64().unwrap() > 0);
        let bad = client
            .post(format!("{base}/v1/messages"))
            .header("content-type", "application/json")
            .body("broken")
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert_eq!(bad.json::<Value>().await.unwrap()["type"], "error");
        task.abort();
        up_task.abort();
    }

    #[test]
    fn config_defaults_validation_and_file_loading() {
        let c = Config::from_values(&HashMap::new()).unwrap();
        assert_eq!(c.port, 10800);
        assert!(Config::from_values(&HashMap::from([("PROXY_PORT".into(), "0".into())])).is_err());
        assert!(
            Config::from_values(&HashMap::from([(
                "OPENCODE_GO_ENABLED".into(),
                "invalid".into()
            )]))
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path,"OPENCODE_GO_ENABLED=true\nOPENCODE_API_KEY=test-value\nMODEL_PROTOCOL_OVERRIDES='{\"new-model\":\"responses\"}'\n").unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.providers[0].key, "test-value");
        assert_eq!(c.protocols["new-model"], Protocol::Responses);
        assert_eq!(
            selected_protocol("opencode-go", "new-model", &c.protocols),
            Protocol::Responses
        );
        assert!(
            Config::from_values(&HashMap::from([(
                "MODEL_PROTOCOL_OVERRIDES".into(),
                r#"{"model":"typo"}"#.into()
            )]))
            .is_err()
        );
    }
}
mod translation {
    use crate::{
        provider::{Protocol, identifier, protocol, valid_identifier},
        stream::{Decoder, Translator},
        translate,
    };
    use serde_json::{Value, json};

    #[test]
    fn messages_images_tools_system_and_choices() {
        let req = json!({"model":"proxy/model","system":[{"type":"text","text":"one"},{"type":"text","text":"two"}],"max_tokens":64,"stream":true,
        "messages":[{"role":"user","content":"hi"},{"role":"system","content":"middle"},
        {"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"read","input":{"path":"a"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":[{"type":"text","text":"done"}]},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}]}],
        "tools":[{"name":"read","input_schema":{"type":"object"}}],"tool_choice":{"type":"tool","name":"read","disable_parallel_tool_use":true}});
        let chat = translate::request(&req, "raw", Protocol::Chat).unwrap();
        assert_eq!(chat["messages"][0]["content"], "one\ntwo");
        assert_eq!(chat["messages"][2]["role"], "system");
        assert_eq!(
            chat["messages"][3]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"a"}"#
        );
        assert_eq!(chat["messages"][4]["role"], "tool");
        assert_eq!(
            chat["messages"][5]["content"][0]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(chat["parallel_tool_calls"], false);
        assert_eq!(chat["stream_options"]["include_usage"], true);
        let responses = translate::request(&req, "raw", Protocol::Responses).unwrap();
        assert_eq!(responses["input"][3]["type"], "function_call");
        assert_eq!(responses["input"][4]["type"], "function_call_output");
        assert_eq!(
            responses["tool_choice"],
            json!({"type":"function","name":"read"})
        );
        assert_eq!(responses["store"], false);
    }

    #[test]
    fn nonstream_text_tools_usage_and_limits() {
        let chat = json!({"choices":[{"message":{"content":"hi","tool_calls":[{"id":"c1","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":4}});
        let a = translate::response(&chat, "proxy/raw", Protocol::Chat).unwrap();
        assert_eq!(a["stop_reason"], "tool_use");
        assert_eq!(a["content"][1]["input"]["path"], "a");
        assert_eq!(a["usage"]["input_tokens"], 10);
        let responses = json!({"status":"incomplete","output":[{"type":"message","content":[{"type":"output_text","text":"hi"}]}],"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":12,"output_tokens":3}});
        let b = translate::response(&responses, "proxy/raw", Protocol::Responses).unwrap();
        assert_eq!(b["stop_reason"], "max_tokens");
        assert_eq!(b["usage"]["input_tokens"], 12);
        assert!(translate::response(&json!({}), "x", Protocol::Chat).is_err());
    }

    #[test]
    fn sse_framing_handles_split_unicode_crlf_and_multiline() {
        let raw = "event: x\r\ndata: {\"text\":\"español\",\r\ndata: \"n\":1}\r\n\r\n";
        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        for byte in raw.as_bytes() {
            frames.extend(decoder.push(&[*byte], false).unwrap());
        }
        assert_eq!(frames.len(), 1);
        let v: Value = serde_json::from_str(&frames[0]).unwrap();
        assert_eq!(v["text"], "español");
    }

    #[test]
    fn responses_stream_does_not_duplicate_tool_arguments() {
        let mut state = Translator::new(Protocol::Responses);
        let added = json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"item1","call_id":"call1","name":"read","arguments":""}});
        let mut events = state.accept(&added.to_string()).unwrap();
        events.extend(state.accept(&json!({"type":"response.function_call_arguments.delta","item_id":"item1","delta":"{\"path\":"}).to_string()).unwrap());
        events.extend(state.accept(&json!({"type":"response.function_call_arguments.delta","item_id":"item1","delta":"\"a\"}"}).to_string()).unwrap());
        events.extend(state.accept(&json!({"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","id":"item1","call_id":"call1","name":"read","arguments":"{\"path\":\"a\"}"}}).to_string()).unwrap());
        state.accept(&json!({"type":"response.completed","response":{"usage":{"input_tokens":5,"output_tokens":4}}}).to_string()).unwrap();
        events.extend(state.finish().unwrap());
        let args: String = events
            .iter()
            .filter_map(|v| v["delta"]["partial_json"].as_str())
            .collect();
        assert_eq!(args, r#"{"path":"a"}"#);
        assert_eq!(events[events.len() - 2]["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[events.len() - 2]["usage"]["input_tokens"], 5);
    }

    #[test]
    fn done_only_arguments_and_unique_block_indexes() {
        let mut state = Translator::new(Protocol::Responses);
        let mut events = state
            .accept(&json!({"type":"response.output_text.delta","delta":"before"}).to_string())
            .unwrap();
        events.extend(state.accept(&json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"i","call_id":"c","name":"f","arguments":"{}"}}).to_string()).unwrap());
        events.extend(
            state
                .accept(&json!({"type":"response.output_text.delta","delta":"after"}).to_string())
                .unwrap(),
        );
        let indexes: Vec<_> = events
            .iter()
            .filter(|v| v["type"] == "content_block_start")
            .map(|v| v["index"].as_u64().unwrap())
            .collect();
        assert_eq!(indexes, vec![0, 1, 2]);
        assert!(state.finish().is_err());
        assert_eq!(
            state
                .accept(
                    &json!({"type":"response.failed","response":{"error":{"message":"failed"}}})
                        .to_string()
                )
                .unwrap()[0]["error"]["message"],
            "failed"
        );
    }

    #[test]
    fn chat_stream_buffers_arguments_until_tool_metadata_arrives() {
        let mut state = Translator::new(Protocol::Chat);
        let first = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"x\":"}}]}}]});
        assert!(state.accept(&first.to_string()).unwrap().is_empty());
        let next = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"f","arguments":"1}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":8,"completion_tokens":5}});
        let events = state.accept(&next.to_string()).unwrap();
        assert_eq!(events[1]["delta"]["partial_json"], r#"{"x":1}"#);
        let final_events = state.finish().unwrap();
        assert_eq!(final_events[0]["usage"]["input_tokens"], 8);
    }

    #[test]
    fn reasoning_controls_are_model_specific_and_preserve_generic_requests() {
        let req = json!({"max_tokens":512,"temperature":0.7,"top_p":0.9,"stop_sequences":["STOP"],
            "thinking":{"type":"enabled","budget_tokens":2048},"output_config":{"effort":"max"},
            "messages":[{"role":"user","content":"hi"}]});
        for (model, effort) in [
            ("o3", "high"),
            ("gpt-5", "high"),
            ("gpt-5.2", "xhigh"),
            ("gpt-5.4-2026-03-05", "xhigh"),
            ("gpt-5.6-luna", "max"),
            ("gpt-6.1-sol", "max"),
        ] {
            for protocol in [Protocol::Chat, Protocol::Responses] {
                let body = translate::request(&req, model, protocol).unwrap();
                if protocol == Protocol::Chat {
                    assert_eq!(body["reasoning_effort"], effort);
                    assert_eq!(body["max_completion_tokens"], 512);
                    assert!(body.get("max_tokens").is_none());
                } else {
                    assert_eq!(body["reasoning"]["effort"], effort);
                    assert_eq!(body["max_output_tokens"], 512);
                }
                for field in [
                    "thinking",
                    "effort",
                    "output_config",
                    "temperature",
                    "top_p",
                    "stop",
                ] {
                    assert!(body.get(field).is_none(), "{model}: {field}");
                }
            }
        }
        for model in [
            "unknown",
            "gpt-4o",
            "gpt-5-chat-latest",
            "gpt-5.4-pro",
            "gpt-5.4-custom",
            "muse-spark-1.3-contributor",
            "grok-4.7",
        ] {
            for protocol in [Protocol::Chat, Protocol::Responses] {
                let body = translate::request(&req, model, protocol).unwrap();
                assert!(body.get("reasoning").is_none());
                assert!(body.get("reasoning_effort").is_none());
                assert!(body.get("thinking").is_none());
                assert_eq!(body["temperature"], 0.7);
                if protocol == Protocol::Chat {
                    assert_eq!(body["max_tokens"], 512);
                }
            }
        }
    }

    #[test]
    fn thinking_defaults_disabled_modes_and_tool_protocol_constraints() {
        let mut req = json!({"messages":[{"role":"user","content":"hi"}],"max_tokens":512,
            "temperature":0.5,"top_p":0.8,"thinking":{"type":"adaptive"}});
        for mode in ["enabled", "adaptive"] {
            req["thinking"] = json!({"type":mode,"budget_tokens":4096});
            assert_eq!(
                translate::request(&req, "gpt-5.4", Protocol::Responses).unwrap()["reasoning"]["effort"],
                "medium"
            );
        }
        req["effort"] = json!("low");
        assert_eq!(
            translate::request(&req, "o3", Protocol::Chat).unwrap()["reasoning_effort"],
            "low"
        );
        req["output_config"] = json!({"effort":"high"});
        assert_eq!(
            translate::request(&req, "o3", Protocol::Chat).unwrap()["reasoning_effort"],
            "high"
        );
        req["thinking"] = json!({"type":"disabled"});
        for model in ["gpt-5.1", "gpt-5.4", "gpt-6-luna"] {
            let body = translate::request(&req, model, Protocol::Chat).unwrap();
            assert_eq!(body["reasoning_effort"], "none");
            assert_eq!(body["temperature"], 0.5);
        }
        for model in ["o3", "gpt-5", "gpt-6-astra", "gpt-6.1-sol"] {
            let body = translate::request(&req, model, Protocol::Responses).unwrap();
            assert!(body.get("reasoning").is_none());
            assert!(body.get("temperature").is_none());
        }
        req["tools"] = json!([{"name":"read","input_schema":{"type":"object"}}]);
        assert!(
            translate::request(&req, "gpt-6-astra", Protocol::Chat)
                .unwrap_err()
                .to_string()
                .contains("selecciona Responses")
        );
        assert!(translate::request(&req, "gpt-6-luna", Protocol::Chat).is_ok());
        req["thinking"] = json!({"type":"adaptive"});
        assert!(translate::request(&req, "gpt-6-luna", Protocol::Chat).is_err());
        assert!(translate::request(&req, "gpt-6-luna", Protocol::Responses).is_ok());
    }

    #[test]
    fn thinking_is_preserved_natively_and_never_fabricated_across_protocols() {
        let thinking = json!([{"type":"thinking","thinking":"private","signature":"native-signature"},
            {"type":"redacted_thinking","data":"opaque"}]);
        let req = json!({"model":"proxy/model","thinking":{"type":"adaptive"},"output_config":{"effort":"high"},
            "messages":[{"role":"assistant","content":thinking},{"role":"user","content":"continue"}]});
        let mut native = req.clone();
        native["model"] = json!("raw");
        assert_eq!(
            translate::request(&req, "raw", Protocol::Messages).unwrap(),
            native
        );
        let response = json!({"content":thinking,"stop_reason":"end_turn"});
        assert_eq!(
            translate::response(&response, "raw", Protocol::Messages).unwrap(),
            response
        );
        for protocol in [Protocol::Chat, Protocol::Responses] {
            let body = translate::request(&req, "raw", protocol).unwrap();
            let history = &body[if protocol == Protocol::Chat {
                "messages"
            } else {
                "input"
            }];
            assert_eq!(history.as_array().unwrap().len(), 1);
            assert!(!body.to_string().contains("private"));
            assert!(!body.to_string().contains("signature"));
        }
        let raw = json!({"status":"completed","output":[{"type":"reasoning","id":"rs1","encrypted_content":"secret","summary":[{"type":"summary_text","text":"summary"}]},
            {"type":"message","content":[{"type":"output_text","text":"public"}]}]});
        let result = translate::response(&raw, "raw", Protocol::Responses).unwrap();
        assert_eq!(result["content"][0]["type"], "redacted_thinking");
        assert_eq!(result["content"][1], json!({"type":"text","text":"public"}));
    }

    #[test]
    fn parallel_tools_results_and_next_turns_keep_ids_and_responses_order() {
        let req = json!({"messages":[{"role":"user","content":"start"},
            {"role":"assistant","content":[{"type":"text","text":"before"},
                {"type":"tool_use","id":"c1","name":"read","input":{"path":"a"}},
                {"type":"text","text":"between"},
                {"type":"tool_use","id":"c2","name":"read","input":{"path":"b"}},
                {"type":"text","text":"after"}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"A"},
                {"type":"tool_result","tool_use_id":"c2","content":"B","is_error":true}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"c3","name":"write","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"c3","content":"done"}]}],
            "tool_choice":{"type":"auto","disable_parallel_tool_use":false}});
        let chat = translate::request(&req, "raw", Protocol::Chat).unwrap();
        assert_eq!(
            chat["messages"][1]["tool_calls"].as_array().unwrap().len(),
            2
        );
        assert_eq!(chat["messages"][2]["tool_call_id"], "c1");
        assert_eq!(chat["messages"][3]["tool_call_id"], "c2");
        assert_eq!(chat["messages"][5]["tool_call_id"], "c3");
        assert_eq!(chat["parallel_tool_calls"], true);
        let responses = translate::request(&req, "raw", Protocol::Responses).unwrap();
        let input = responses["input"].as_array().unwrap();
        assert_eq!(input[1]["content"][0]["text"], "before");
        assert_eq!(input[2]["call_id"], "c1");
        assert_eq!(input[3]["content"][0]["text"], "between");
        assert_eq!(input[4]["call_id"], "c2");
        assert_eq!(input[5]["content"][0]["text"], "after");
        for (index, id, output) in [
            (6, "c1", "A"),
            (7, "c2", "[Tool execution failed; is_error=true]\nB"),
            (9, "c3", "done"),
        ] {
            assert_eq!(
                input[index],
                json!({"type":"function_call_output","call_id":id,"output":output})
            );
        }
    }

    #[test]
    fn responses_reasoning_roundtrips_multiple_items_in_original_order() {
        let model = "openai/gpt-6-luna";
        let first = json!({"type":"reasoning","id":"rs1","status":"completed","summary":[{"type":"summary_text","text":"summary"}],"content":[],"encrypted_content":"opaque-1"});
        let second =
            json!({"type":"reasoning","id":"rs2","summary":[],"encrypted_content":"opaque-2"});
        let raw = json!({"status":"completed","output":[first,
            {"type":"message","content":[{"type":"output_text","text":"between"}]},second,
            {"type":"function_call","call_id":"c1","name":"read","arguments":"{}"}]});
        let response = translate::response(&raw, model, Protocol::Responses).unwrap();
        assert!(!response["content"].to_string().contains("opaque-"));
        let req = json!({"model":model,"messages":[{"role":"assistant","content":response["content"]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"done"}]}]});
        let body = translate::request(&req, "gpt-6-luna", Protocol::Responses).unwrap();
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["store"], false);
        assert_eq!(body["input"][0], first);
        assert_eq!(body["input"][1]["content"][0]["text"], "between");
        assert_eq!(body["input"][2], second);
        assert_eq!(body["input"][3]["call_id"], "c1");
        assert_eq!(body["input"][4]["type"], "function_call_output");
        // Another conversation has no state unless its client supplied it.
        let fresh = translate::request(
            &json!({"model":model,"messages":[{"role":"user","content":"hi"}]}),
            "gpt-6-luna",
            Protocol::Responses,
        )
        .unwrap();
        assert!(!fresh.to_string().contains("opaque-"));
    }

    #[test]
    fn proxy_state_is_bound_to_public_model_and_never_sent_as_native_thinking() {
        let capsule = translate::state_block(
            "openai/gpt-6-luna",
            "responses",
            &json!({"type":"reasoning","summary":[],"encrypted_content":"secret"}),
        )
        .unwrap();
        let native = json!({"type":"redacted_thinking","data":"native-ciphertext"});
        let mut req = json!({"model":"other/gpt-6-luna","messages":[{"role":"assistant","content":[capsule,native,{"type":"text","text":"answer"}]}]});
        let body = translate::request(&req, "gpt-6-luna", Protocol::Responses).unwrap();
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert!(!body.to_string().contains("secret"));
        req["model"] = json!("openai/gpt-6-luna");
        let chat = translate::request(&req, "raw", Protocol::Chat).unwrap();
        assert_eq!(
            chat["messages"][0],
            json!({"role":"assistant","content":"answer"})
        );
        let messages = translate::request(&req, "claude", Protocol::Messages).unwrap();
        assert_eq!(
            messages["messages"][0]["content"],
            json!([native,{"type":"text","text":"answer"}])
        );
        req["messages"][0]["content"] =
            json!([{"type":"redacted_thinking","data":"claude-proxy-state-v1:invalid!"}]);
        assert!(translate::request(&req, "gpt-6-luna", Protocol::Responses).is_err());
        assert!(
            translate::state_block("m", "deepseek", &json!("x".repeat(3 * 1024 * 1024))).is_err()
        );
    }

    #[test]
    fn deepseek_state_and_controls_survive_all_assistant_turns() {
        let model = "opencode-go/deepseek-v4.1-flash";
        let raw = json!({"choices":[{"message":{"reasoning_content":"private thought","content":null,"tool_calls":[{"id":"c1","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]});
        let first = translate::response(&raw, model, Protocol::Chat).unwrap();
        let empty = translate::response(&json!({"choices":[{"message":{"reasoning_content":"","content":"done"},"finish_reason":"stop"}]}), model, Protocol::Chat).unwrap();
        let mut req = json!({"model":model,"thinking":{"type":"adaptive"},"output_config":{"effort":"max"},"temperature":0.5,
            "messages":[{"role":"assistant","content":first["content"]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"ok"}]},
                {"role":"assistant","content":empty["content"]},{"role":"user","content":"continue"}]});
        let body = translate::request(&req, "deepseek-v4.1-flash", Protocol::Chat).unwrap();
        assert_eq!(body["messages"][0]["reasoning_content"], "private thought");
        assert_eq!(body["messages"][0]["content"], "");
        assert_eq!(body["messages"][2]["reasoning_content"], "");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "max");
        assert!(body.get("temperature").is_none());
        req["tool_choice"] = json!({"type":"any"});
        assert!(translate::request(&req, "deepseek-v4.1-flash", Protocol::Chat).is_err());
        req["thinking"]["type"] = json!("disabled");
        let disabled = translate::request(&req, "deepseek-v4.1-flash", Protocol::Chat).unwrap();
        assert_eq!(disabled["thinking"]["type"], "disabled");
        assert!(disabled.get("reasoning_effort").is_none());
        assert_eq!(disabled["temperature"], 0.5);
    }

    #[test]
    fn deepseek_stream_keeps_late_reasoning_before_tools_and_answer() {
        let model = "opencode-go/deepseek-v4.1-flash";
        for tools in [false, true] {
            let mut state = Translator::with_model(Protocol::Chat, model);
            let mut events = state
                .accept(&json!({"choices":[{"delta":{"reasoning_content":"first "}}]}).to_string())
                .unwrap();
            if tools {
                events.extend(state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":""}}]}}]}).to_string()).unwrap());
                events.extend(
                    state
                        .accept(
                            &json!({"choices":[{"delta":{"reasoning_content":"last"}}]})
                                .to_string(),
                        )
                        .unwrap(),
                );
                events.extend(state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]}).to_string()).unwrap());
            } else {
                events.extend(state.accept(&json!({"choices":[{"delta":{"reasoning_content":"last","content":"answer"}}]}).to_string()).unwrap());
                events.extend(state.accept(&json!({"choices":[{"delta":{"reasoning_content":"","content":""},"finish_reason":"stop"}]}).to_string()).unwrap());
            }
            events.extend(state.finish().unwrap());
            let blocks = reconstructed(&events);
            assert_eq!(
                blocks[0],
                translate::state_block(model, "deepseek", &json!("first last")).unwrap()
            );
            assert_eq!(blocks[1]["type"], if tools { "tool_use" } else { "text" });
            let req = json!({"model":model,"messages":[{"role":"assistant","content":blocks}]});
            assert_eq!(
                translate::request(&req, "deepseek-v4.1-flash", Protocol::Chat).unwrap()["messages"]
                    [0]["reasoning_content"],
                "first last"
            );
        }
    }

    #[test]
    fn responses_stream_emits_only_complete_reasoning_and_deduplicates_final_output() {
        let model = "openai/gpt-6-luna";
        let mut state = Translator::with_model(Protocol::Responses, model);
        let added = json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs1","encrypted_content":"partial"}});
        assert!(state.accept(&added.to_string()).unwrap().is_empty());
        let reasoning =
            json!({"type":"reasoning","id":"rs1","summary":[],"encrypted_content":"complete"});
        let mut events = state
            .accept(
                &json!({"type":"response.output_item.done","output_index":0,"item":reasoning})
                    .to_string(),
            )
            .unwrap();
        events.extend(
            state
                .accept(
                    &json!({"type":"response.output_text.delta","output_index":1,"delta":"answer"})
                        .to_string(),
                )
                .unwrap(),
        );
        events.extend(state.accept(&json!({"type":"response.completed","response":{"status":"completed","output":[reasoning,{"type":"message","content":[{"type":"output_text","text":"answer"}]}]}}).to_string()).unwrap());
        events.extend(state.finish().unwrap());
        let blocks = reconstructed(&events);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            translate::responses_state(model, &reasoning)
                .unwrap()
                .unwrap()
        );
        assert_eq!(blocks[1], json!({"type":"text","text":"answer"}));
    }

    // Reconstruct as an Anthropic SSE client would, checking block lifecycle
    // and JSON validity in addition to the final values.
    pub(super) fn reconstructed(events: &[Value]) -> Vec<Value> {
        let mut blocks: Vec<Value> = Vec::new();
        let mut active = None;
        let mut arguments = String::new();
        for event in events {
            match event["type"].as_str().unwrap() {
                "content_block_start" => {
                    assert!(active.is_none(), "overlapping blocks: {events:?}");
                    let index = event["index"].as_u64().unwrap() as usize;
                    assert_eq!(index, blocks.len());
                    blocks.push(event["content_block"].clone());
                    active = Some(index);
                }
                "content_block_delta" => {
                    let index = event["index"].as_u64().unwrap() as usize;
                    assert_eq!(active, Some(index));
                    if let Some(delta) = event["delta"]["partial_json"].as_str() {
                        arguments.push_str(delta);
                    } else {
                        let mut text = blocks[index]["text"].as_str().unwrap().to_string();
                        text.push_str(event["delta"]["text"].as_str().unwrap());
                        blocks[index]["text"] = json!(text);
                    }
                }
                "content_block_stop" => {
                    let index = event["index"].as_u64().unwrap() as usize;
                    assert_eq!(active.take(), Some(index));
                    if blocks[index]["type"] == "tool_use" {
                        blocks[index]["input"] = serde_json::from_str(&arguments).unwrap();
                        assert!(blocks[index]["input"].is_object());
                        arguments.clear();
                    }
                }
                "message_delta" | "message_stop" => assert!(active.is_none()),
                other => panic!("unexpected event: {other}"),
            }
        }
        assert!(active.is_none());
        blocks
    }

    #[test]
    fn chat_parallel_fragments_and_reasoning_produce_complete_consecutive_blocks() {
        let frames = [
            json!({"choices":[{"delta":{"reasoning_content":"private"}}]}),
            json!({"choices":[{"delta":{"content":"working"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"re","arguments":""}},
                {"index":1,"function":{"arguments":"{\"path\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c2","function":{"name":"read","arguments":"\"b\"}"}},
                {"index":0,"function":{"name":"ad","arguments":"{\"path\":\"español\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7}}),
        ];
        let mut state = Translator::new(Protocol::Chat);
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        let wire = frames
            .iter()
            .map(|v| format!("data: {v}\r\n\r\n"))
            .collect::<String>()
            + "data: [DONE]\r\n\r\n";
        for byte in wire.as_bytes() {
            for frame in decoder.push(&[*byte], false).unwrap() {
                events.extend(state.accept(&frame).unwrap());
            }
        }
        events.extend(state.finish().unwrap());
        assert_eq!(
            reconstructed(&events),
            vec![
                json!({"type":"text","text":"working"}),
                translate::state_block(
                    "",
                    "chat_reasoning",
                    &json!({"reasoning_content":"private"})
                )
                .unwrap(),
                json!({"type":"tool_use","id":"c1","name":"read","input":{"path":"español"}}),
                json!({"type":"tool_use","id":"c2","name":"read","input":{"path":"b"}})
            ]
        );
        assert_eq!(
            events[events.len() - 2]["usage"],
            json!({"input_tokens":11,"output_tokens":7})
        );
        assert_eq!(events[events.len() - 2]["delta"]["stop_reason"], "tool_use");
        assert!(state.finish().is_err());
    }

    #[test]
    fn responses_parallel_early_arguments_and_all_done_variants_do_not_duplicate() {
        let call1 = json!({"type":"function_call","id":"i1","call_id":"c1","name":"read","arguments":"{\"path\":\"a\"}"});
        let call2 = json!({"type":"function_call","id":"i2","call_id":"c2","name":"read","arguments":"{\"path\":\"b\"}"});
        let frames = [
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs"}}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"private summary"}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"i1","delta":"{\"path\":"}),
            json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"i2","call_id":"c2","name":"read","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","item_id":"i2","delta":"{\"path\":\"b\"}"}),
            json!({"type":"response.output_item.done","output_index":2,"item":call2}),
            json!({"type":"response.function_call_arguments.done","output_index":1,"item_id":"i1","arguments":"{\"path\":\"a\"}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":call1}),
            json!({"type":"response.output_item.done","output_index":2,"item":call2}),
            json!({"type":"response.completed","response":{"output":[{"type":"reasoning","id":"rs","encrypted_content":"opaque"},call1,call2],"usage":{"input_tokens":10,"output_tokens":6}}}),
        ];
        let mut state = Translator::new(Protocol::Responses);
        let mut events = Vec::new();
        for frame in frames {
            events.extend(state.accept(&frame.to_string()).unwrap());
        }
        events.extend(state.finish().unwrap());
        let blocks = reconstructed(&events);
        assert_eq!(
            blocks
                .iter()
                .filter(|b| b["type"] == "redacted_thinking")
                .count(),
            1
        );
        assert_eq!(
            blocks
                .into_iter()
                .filter(|b| b["type"] == "tool_use")
                .collect::<Vec<_>>(),
            vec![
                json!({"type":"tool_use","id":"c1","name":"read","input":{"path":"a"}}),
                json!({"type":"tool_use","id":"c2","name":"read","input":{"path":"b"}})
            ]
        );
        assert_eq!(events[events.len() - 2]["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn responses_final_output_only_and_token_limit_match_nonstreaming() {
        let raw = json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},
            "output":[{"type":"function_call","id":"i","call_id":"c","name":"read","arguments":"{}"}],
            "usage":{"input_tokens":3,"output_tokens":9}});
        let result = translate::response(&raw, "raw", Protocol::Responses).unwrap();
        assert_eq!(result["stop_reason"], "max_tokens");
        let mut state = Translator::new(Protocol::Responses);
        let mut events = state
            .accept(&json!({"type":"response.incomplete","response":raw}).to_string())
            .unwrap();
        events.extend(state.finish().unwrap());
        assert_eq!(json!(reconstructed(&events)), result["content"]);
        assert_eq!(
            events[events.len() - 2]["delta"]["stop_reason"],
            result["stop_reason"]
        );
    }

    #[test]
    fn failed_truncated_or_invalid_calls_cannot_finish_successfully() {
        for args in ["{", "[]", "null", "1"] {
            let mut state = Translator::new(Protocol::Chat);
            let frame = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"read","arguments":args}}]},"finish_reason":"tool_calls"}]});
            assert!(state.accept(&frame.to_string()).is_err(), "{args}");
            assert!(state.finish().is_err());
            assert!(state.accept("[DONE]").is_err());
        }
        let mut state = Translator::new(Protocol::Chat);
        state.accept("[DONE]").unwrap();
        assert_eq!(
            state
                .accept(&json!({"error":{"message":"late error"}}).to_string())
                .unwrap()[0]["error"]["message"],
            "late error"
        );
        assert!(state.finish().is_err());
        let mut state = Translator::new(Protocol::Responses);
        state.accept(&json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"i","call_id":"c","name":"read","arguments":""}}).to_string()).unwrap();
        assert!(state.finish().is_err());
        state
            .accept(&json!({"type":"response.completed"}).to_string())
            .unwrap();
        assert!(state.finish().is_err());
        let mut state = Translator::new(Protocol::Responses);
        assert!(state.accept("[DONE]").is_err());
        assert!(state.finish().is_err());
    }

    #[test]
    fn nonstream_invalid_tools_are_rejected_and_responses_id_fallback_is_consistent() {
        for protocol in [Protocol::Chat, Protocol::Responses] {
            for (id, name, arguments) in [
                (json!("c"), json!("read"), json!(1)),
                (json!("c"), json!("read"), json!("[]")),
                (json!(null), json!("read"), json!("{}")),
                (json!("c"), json!(""), json!("{}")),
            ] {
                let raw = if protocol == Protocol::Chat {
                    json!({"choices":[{"message":{"tool_calls":[{"id":id,"function":{"name":name,"arguments":arguments}}]},"finish_reason":"tool_calls"}]})
                } else {
                    json!({"status":"completed","output":[{"type":"function_call","call_id":id,"name":name,"arguments":arguments}]})
                };
                assert!(translate::response(&raw, "raw", protocol).is_err());
            }
        }
        let raw = json!({"status":"completed","output":[{"type":"function_call","id":"fallback","call_id":null,"name":"read","arguments":"{}"}]});
        let native = translate::response(&raw, "raw", Protocol::Responses).unwrap();
        let mut state = Translator::new(Protocol::Responses);
        let mut events = state
            .accept(&json!({"type":"response.completed","response":raw}).to_string())
            .unwrap();
        events.extend(state.finish().unwrap());
        assert_eq!(json!(reconstructed(&events)), native["content"]);
        assert_eq!(native["content"][0]["id"], "fallback");
    }

    #[test]
    fn routing_and_opencode_identifier_format() {
        for m in ["muse-spark-1.3-contributor", "grok-4.7", "gpt-6-luna"] {
            assert_eq!(protocol(m), Protocol::Responses);
        }
        for m in ["claude-haiku-5-5", "minimax-m3", "qwen3.8-flash"] {
            assert_eq!(protocol(m), Protocol::Messages);
        }
        assert_eq!(protocol("deepseek-v4.1-flash"), Protocol::Chat);
        let a = identifier("ses", true);
        let b = identifier("ses", true);
        assert!(valid_identifier(&a, "ses"));
        assert_ne!(a, b);
        assert!(!valid_identifier("uuid", "ses"));
        assert!(valid_identifier(&identifier("msg", false), "msg"));
    }
}
