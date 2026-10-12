use crate::{provider::Protocol, stream::Translator, translate};
use serde_json::{Value, json};

fn request(content: Value) -> Value {
    json!({"model":"test/raw","max_tokens":128,"messages":[{"role":"user","content":content}]})
}

fn streamed(protocol: Protocol, frames: Vec<Value>) -> Value {
    let mut state = Translator::with_model(protocol, "test/raw");
    let mut events = Vec::new();
    for frame in frames {
        events.extend(state.accept(&frame.to_string()).unwrap());
    }
    events.extend(state.finish().unwrap());
    reconstruct(&events)
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

#[test]
fn parallel_tool_results_preserve_images_files_errors_and_adjacency() {
    let image =
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"YWJj"}});
    let pdf = json!({"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"JVBERg=="}});
    let req = request(json!([
        {"type":"tool_result","tool_use_id":"c1","is_error":true,"content":[{"type":"text","text":"failed"},image,pdf]},
        {"type":"tool_result","tool_use_id":"c2","content":"second"},
        {"type":"text","text":"continue"}
    ]));
    let responses = translate::request(&req, "raw", Protocol::Responses).unwrap();
    let output = &responses["input"][0]["output"];
    assert!(
        output[0]["text"]
            .as_str()
            .unwrap()
            .contains("is_error=true")
    );
    assert_eq!(output[2]["image_url"], "data:image/png;base64,YWJj");
    assert_eq!(
        output[3]["file_data"],
        "data:application/pdf;base64,JVBERg=="
    );
    let chat = translate::request(&req, "raw", Protocol::Chat).unwrap();
    assert_eq!(chat["messages"][0]["role"], "tool");
    assert_eq!(chat["messages"][1]["tool_call_id"], "c2");
    assert_eq!(chat["messages"][2]["role"], "user");
    assert_eq!(
        chat["messages"][2]["content"][3]["image_url"]["url"],
        "data:image/png;base64,YWJj"
    );
    assert_eq!(
        chat["messages"][2]["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["text"],
        "continue"
    );
}

#[test]
fn documents_search_results_and_tool_references_keep_content_and_sources() {
    let mut req = request(json!([
        {"type":"document","title":"Guide","context":"Reference only","source":{"type":"text","media_type":"text/plain","data":"full document"}},
        {"type":"search_result","title":"Result","source":"https://example.com","content":[{"type":"text","text":"found text"}]},
        {"type":"tool_reference","tool_name":"read"}
    ]));
    req["tools"] = json!([{"name":"read","input_schema":{"type":"object"},"defer_loading":true}]);
    for protocol in [Protocol::Chat, Protocol::Responses] {
        let body = translate::request(&req, "raw", protocol)
            .unwrap()
            .to_string();
        for expected in [
            "Guide",
            "Reference only",
            "full document",
            "https://example.com",
            "found text",
            "Available tool: read",
        ] {
            assert!(body.contains(expected));
        }
    }
    let url = request(
        json!([{"type":"document","source":{"type":"url","url":"https://example.com/a.pdf"}}]),
    );
    assert_eq!(
        translate::request(&url, "raw", Protocol::Responses).unwrap()["input"][0]["content"][0]["file_url"],
        "https://example.com/a.pdf"
    );
    assert!(translate::request(&url, "raw", Protocol::Chat).is_err());
}

#[test]
fn structured_outputs_strict_tools_and_examples_are_translated() {
    let mut req = request(json!("answer"));
    let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false});
    req["output_config"] = json!({"format":{"type":"json_schema","schema":schema}});
    req["tools"] = json!([{"name":"read","strict":true,"input_schema":schema,"input_examples":[{"ok":true}]},{"name":"other","input_schema":{"type":"object"}}]);
    let chat = translate::request(&req, "raw", Protocol::Chat).unwrap();
    assert_eq!(chat["response_format"]["json_schema"]["schema"], schema);
    assert_eq!(chat["tools"][0]["function"]["strict"], true);
    assert!(
        chat["tools"][0]["function"]["description"]
            .as_str()
            .unwrap()
            .contains("Input examples")
    );
    let responses = translate::request(&req, "raw", Protocol::Responses).unwrap();
    assert_eq!(responses["text"]["format"]["schema"], schema);
    assert_eq!(responses["tools"][1]["strict"], false);
}

#[test]
fn incompatible_semantic_features_fail_explicitly_and_messages_stays_native() {
    let bad = [
        request(json!([{"type":"unknown","data":"important"}])),
        request(
            json!([{"type":"tool_result","tool_use_id":"c","content":[{"type":"unknown","data":"important"}]}]),
        ),
        {
            let mut req = request(json!("hi"));
            req["tools"] = json!([{"name":"web_search","type":"web_search_20250305"}]);
            req
        },
        {
            let mut req = request(json!("hi"));
            req["context_management"] = json!({"edits":[{"type":"clear_tool_uses_20250919"}]});
            req
        },
    ];
    for req in bad {
        for protocol in [Protocol::Chat, Protocol::Responses] {
            assert!(translate::request(&req, "raw", protocol).is_err());
        }
        let mut expected = req.clone();
        expected["model"] = json!("raw");
        assert_eq!(
            translate::request(&req, "raw", Protocol::Messages).unwrap(),
            expected
        );
    }
}

#[test]
fn refusal_text_and_finish_reason_survive_json_and_streaming() {
    for protocol in [Protocol::Chat, Protocol::Responses] {
        let raw = if protocol == Protocol::Chat {
            json!({"choices":[{"message":{"content":null,"refusal":"Cannot help"},"finish_reason":"stop"}]})
        } else {
            json!({"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":"Cannot help"}]}]})
        };
        let converted = translate::response(&raw, "test/raw", protocol).unwrap();
        let frames = if protocol == Protocol::Chat {
            vec![
                json!({"choices":[{"delta":{"refusal":"Cannot "}}]}),
                json!({"choices":[{"delta":{"refusal":"help"},"finish_reason":"stop"}]}),
            ]
        } else {
            vec![
                json!({"type":"response.refusal.delta","delta":"Cannot "}),
                json!({"type":"response.refusal.done","refusal":"Cannot help"}),
                json!({"type":"response.completed","response":raw}),
            ]
        };
        let stream = streamed(protocol, frames);
        assert_eq!(converted["content"], stream["content"]);
        assert_eq!(stream["stop_reason"], "refusal");
    }
}

#[test]
fn final_text_fallback_does_not_duplicate_deltas_or_annotations() {
    let part = json!({"type":"output_text","text":"hello world","annotations":[{"type":"url_citation","url":"https://example.com","title":"Source","start_index":0,"end_index":5}]});
    let item = json!({"type":"message","content":[part]});
    let raw = json!({"status":"completed","output":[item]});
    let final_message = translate::response(&raw, "test/raw", Protocol::Responses).unwrap();
    let stream = streamed(
        Protocol::Responses,
        vec![
            json!({"type":"response.output_text.delta","delta":"hello"}),
            json!({"type":"response.content_part.done","part":part}),
            json!({"type":"response.output_item.done","item":item}),
            json!({"type":"response.completed","response":raw}),
        ],
    );
    assert_eq!(stream["content"], final_message["content"]);
    let mut translator = Translator::new(Protocol::Responses);
    translator
        .accept(&json!({"type":"response.output_text.delta","delta":"hello"}).to_string())
        .unwrap();
    assert!(
        translator
            .accept(&json!({"type":"response.output_text.done","text":"different"}).to_string())
            .is_err()
    );
}

#[test]
fn cache_and_reasoning_usage_match_between_json_and_streaming_without_double_counting() {
    for protocol in [Protocol::Chat, Protocol::Responses] {
        let usage = if protocol == Protocol::Chat {
            json!({"prompt_tokens":100,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":20},"completion_tokens_details":{"reasoning_tokens":5}})
        } else {
            json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":20},"output_tokens_details":{"reasoning_tokens":5}})
        };
        let raw = if protocol == Protocol::Chat {
            json!({"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}],"usage":usage})
        } else {
            json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"OK"}]}],"usage":usage})
        };
        let frames = if protocol == Protocol::Chat {
            vec![
                json!({"choices":[{"delta":{"content":"OK"},"finish_reason":"stop"}]}),
                json!({"choices":[],"usage":usage}),
            ]
        } else {
            vec![json!({"type":"response.completed","response":raw})]
        };
        let converted = translate::response(&raw, "test/raw", protocol).unwrap();
        let stream = streamed(protocol, frames);
        assert_eq!(stream["usage"], converted["usage"]);
        assert_eq!(stream["usage"]["input_tokens"], 80);
        assert_eq!(stream["usage"]["cache_read_input_tokens"], 20);
        assert_eq!(
            stream["usage"]["output_tokens_details"]["reasoning_tokens"],
            5
        );
    }
}

#[test]
fn generic_reasoning_and_signatures_survive_multiple_turns() {
    let details =
        json!([{"type":"reasoning.text","text":"step one","signature":"sig","id":"r","index":0}]);
    let raw = json!({"choices":[{"message":{"content":"OK","reasoning":"thought","reasoning_details":details},"finish_reason":"stop"}]});
    let native = translate::response(&raw, "test/raw", Protocol::Chat).unwrap();
    let stream = streamed(
        Protocol::Chat,
        vec![
            json!({"choices":[{"delta":{"reasoning":"thought","reasoning_details":details}}]}),
            json!({"choices":[{"delta":{"content":"OK"},"finish_reason":"stop"}]}),
        ],
    );
    for answer in [native, stream] {
        let req = json!({"model":"test/raw","messages":[{"role":"assistant","content":answer["content"]},{"role":"user","content":"continue"}]});
        let body = translate::request(&req, "raw", Protocol::Chat).unwrap();
        assert_eq!(body["messages"][0]["reasoning"], "thought");
        assert_eq!(body["messages"][0]["reasoning_details"], details);
        assert_eq!(body["messages"][0]["content"], "OK");
    }
}

#[test]
fn prompt_cache_boundaries_are_mapped_only_for_recognized_models() {
    let req = request(
        json!([{"type":"text","text":"static","cache_control":{"type":"ephemeral","ttl":"1h"}},{"type":"text","text":"dynamic"}]),
    );
    let converted = translate::request(&req, "gpt-6.1-sol", Protocol::Responses).unwrap();
    assert_eq!(
        converted["input"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
        "explicit"
    );
    assert_eq!(converted["prompt_cache_options"]["mode"], "explicit");
    assert!(
        !translate::request(&req, "custom-alias", Protocol::Chat)
            .unwrap()
            .to_string()
            .contains("prompt_cache_breakpoint")
    );
}

#[test]
fn local_stop_sequences_handle_split_unicode_and_remove_subsequent_tools() {
    let req = json!({"stop_sequences":["🛑END"]});
    let mut filter = translate::StopFilter::new(&req).unwrap();
    assert_eq!(filter.push("hello 🛑"), "hello ");
    assert_eq!(filter.push("EN"), "");
    assert_eq!(filter.push("Drest"), "");
    assert_eq!(filter.matched.as_deref(), Some("🛑END"));
    let mut response = json!({"content":[{"type":"text","text":"hello 🛑"},{"type":"text","text":"ENDrest"},{"type":"tool_use","id":"c","name":"read","input":{}}]});
    translate::apply_stops(&mut response, &req).unwrap();
    assert_eq!(response["stop_reason"], "stop_sequence");
    assert_eq!(response["stop_sequence"], "🛑END");
    assert!(
        response["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["type"] == "text")
    );
    assert_eq!(
        response["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["text"].as_str().unwrap())
            .collect::<String>(),
        "hello "
    );
}

#[test]
fn effort_updates_stay_in_the_history_instead_of_changing_previous_turns() {
    let req = json!({"model":"test/raw","messages":[{"role":"user","content":"first"},{"role":"assistant","content":"done"},{"role":"system","content":[],"output_config":{"effort":"high"}},{"role":"user","content":"next"}]});
    let converted = translate::request(&req, "gpt-6.1-sol", Protocol::Responses).unwrap();
    assert_eq!(
        converted["input"][2],
        json!({"type":"configuration_update","reasoning":{"effort":"high"}})
    );
    assert_eq!(converted["input"][3]["content"][0]["text"], "next");
    assert!(translate::request(&req, "gpt-6.1-sol", Protocol::Chat).is_err());
}

#[test]
fn long_and_namespaced_tool_names_restore_to_the_original_client_names() {
    let name = format!("mcp.service/{}", "long-name".repeat(12));
    let mut req = request(json!("hi"));
    req["tools"] = json!([{"name":name,"input_schema":{"type":"object"}}]);
    req["tool_choice"] = json!({"type":"tool","name":name});
    req["messages"] = json!([{"role":"assistant","content":[{"type":"tool_use","id":"c","name":name,"input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"ok"}]}]);
    for protocol in [Protocol::Chat, Protocol::Responses] {
        let body = translate::request(&req, "raw", protocol).unwrap();
        let encoded = if protocol == Protocol::Chat {
            &body["tools"][0]["function"]["name"]
        } else {
            &body["tools"][0]["name"]
        };
        let encoded = encoded.as_str().unwrap();
        assert!(encoded.len() <= 64);
        assert!(
            encoded
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        );
        let history_name = if protocol == Protocol::Chat {
            &body["messages"][0]["tool_calls"][0]["function"]["name"]
        } else {
            &body["input"][0]["name"]
        };
        assert_eq!(history_name, encoded);
        let mut response =
            json!({"content":[{"type":"tool_use","name":encoded,"id":"next","input":{}}]});
        translate::restore_tool_names(&mut response, &req);
        assert_eq!(response["content"][0]["name"], name);
    }
}

#[test]
fn responses_message_phase_id_and_annotations_survive_a_tool_continuation() {
    let message = json!({"type":"message","id":"msg_provider","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"I will check","annotations":[{"type":"url_citation","url":"https://example.com","title":"Source"}]}]});
    let raw = json!({"status":"completed","output":[message,{"type":"function_call","call_id":"c","name":"read","arguments":"{}"}]});
    let native = translate::response(&raw, "test/raw", Protocol::Responses).unwrap();
    let stream = streamed(
        Protocol::Responses,
        vec![
            json!({"type":"response.output_text.delta","delta":"I will check"}),
            json!({"type":"response.output_item.done","item":message}),
            json!({"type":"response.completed","response":raw}),
        ],
    );
    for result in [native, stream] {
        let req = json!({"model":"test/raw","messages":[{"role":"assistant","content":result["content"]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"done"}]}]});
        let converted = translate::request(&req, "raw", Protocol::Responses).unwrap();
        assert_eq!(converted["input"][0], message);
        assert_eq!(converted["input"][1]["type"], "function_call");
        assert_eq!(converted["input"].as_array().unwrap().len(), 3);
    }
}

mod http {
    use super::*;
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
