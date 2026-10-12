use crate::{Protocol, stream::Translator, translate};
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

#[test]
fn sse_accepts_bom_cr_lines_and_chunked_crlf() {
    let mut decoder = crate::stream::Decoder::default();
    let mut frames = Vec::new();
    for byte in "\u{feff}data: español\r\ndata: línea\r\n\r\ndata\r\r".as_bytes() {
        frames.extend(decoder.push(&[*byte], false).unwrap());
    }
    assert_eq!(frames, vec!["español\nlínea", ""]);
}
#[test]
fn sse_limits_individual_events_instead_of_entire_network_chunks() {
    let mut decoder = crate::stream::Decoder::default();
    let bytes = "data: ok\n\n".repeat(450_000);
    assert!(bytes.len() > 4 * 1024 * 1024);
    assert_eq!(
        decoder.push(bytes.as_bytes(), false).unwrap().len(),
        450_000
    );
}

#[test]
fn native_stream_conversion_is_portable_and_requires_a_terminal_event() {
    let mut translator = crate::stream::Translator::new(Protocol::Messages);
    let frames = [
        json!({"type":"message_start","message":{"id":"m"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private"}}),
        json!({"type":"message_stop"}),
    ];
    for frame in frames {
        assert_eq!(translator.accept(&frame.to_string()).unwrap(), vec![frame]);
    }
    assert!(translator.finish().unwrap().is_empty());
    let mut truncated = crate::stream::Translator::new(Protocol::Messages);
    assert!(truncated.finish().is_err());
}
