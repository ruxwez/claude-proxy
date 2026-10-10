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
                    let response = stream::response(upstream, protocol, "test".into());
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
                let response=client.post(format!("{base}/v1/messages")).header("x-api-key","client-key").bearer_auth("client-token").header("x-claude-code-session-id","stable-session")
                .json(&json!({"model":format!("opencode-go/{model}"),"messages":[{"role":"user","content":"hi"}],"max_tokens":8,"stream":streaming,"custom_native_field":"preserved"})).send().await.unwrap();
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
        assert!(
            state
                .accept(
                    &json!({"type":"response.failed","response":{"error":{"message":"failed"}}})
                        .to_string()
                )
                .is_err()
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
        assert_eq!(final_events[1]["usage"]["input_tokens"], 8);
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
