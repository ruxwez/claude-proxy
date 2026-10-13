use super::*;

fn input() -> Value {
    json!({"model":"zed/public","max_tokens":128,"stream":false,"messages":[{"role":"user","content":"hola"}]})
}

#[test]
fn envelopes_select_native_protocol_even_for_json_clients() {
    let mut req = input();
    req["thinking"] = json!({"type":"enabled","budget_tokens":32});
    req["tools"] = json!([{"name":"lookup","input_schema":{"type":"object"}}]);
    for provider in [
        Provider::Anthropic,
        Provider::OpenAi,
        Provider::XAi,
        Provider::Google,
    ] {
        let envelope = request(&req, "native", provider).unwrap();
        assert_eq!(envelope["model"], "native");
        assert_eq!(
            envelope["provider"],
            serde_json::to_value(provider).unwrap()
        );
        let native = &envelope["provider_request"];
        if provider == Provider::Google {
            assert_eq!(native["model"], "models/native");
            assert_eq!(
                native["tools"][0]["functionDeclarations"][0]["name"],
                "lookup"
            );
            assert!(native.get("stream").is_none());
        } else {
            assert_eq!(native["stream"], true);
            assert_eq!(native["model"], "native");
        }
        if provider == Provider::Anthropic {
            assert_eq!(native["thinking"], req["thinking"]);
            assert_eq!(native["tools"][0]["name"], req["tools"][0]["name"]);
            assert_eq!(
                native["tools"][0]["input_schema"],
                req["tools"][0]["input_schema"]
            );
            assert_eq!(native["tools"][0]["description"], "");
        }
    }
}

#[test]
fn anthropic_zed_hoists_system_and_normalizes_strings_without_losing_blocks() {
    let cache = json!({"type":"ephemeral","ttl":"1h"});
    let tool = json!({"type":"tool_use","id":"call","name":"lookup","input":{"nested":"system"}});
    let mut req = input();
    req["system"] = json!([{"type":"text","text":"global","cache_control":cache}]);
    req["messages"] = json!([
        {"role":"system","content":"instrucción"},
        {"role":"system","content":[{"type":"text","text":"extra","cache_control":cache}]},
        {"role":"user","content":"pregunta"},
        {"role":"assistant","content":[{"type":"thinking","thinking":"idea","signature":"opaque"},tool]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"respuesta"}]}
    ]);
    let wire = request(&req, "claude", Provider::Anthropic).unwrap();
    let body = &wire["provider_request"];
    assert_eq!(
        body["system"],
        json!([{"type":"text","text":"global","cache_control":cache},{"type":"text","text":"instrucción"},{"type":"text","text":"extra","cache_control":cache}])
    );
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type":"text","text":"pregunta"}])
    );
    assert_eq!(
        body["messages"][1]["content"],
        req["messages"][3]["content"]
    );
    assert_eq!(body["messages"][2]["content"][0]["is_error"], false);
    assert_eq!(
        req["messages"][0]["role"], "system",
        "input must not be mutated"
    );
    assert!(
        warnings(&req, "claude", Provider::Anthropic).contains(&"system_messages_moved_to_system")
    );
    req["system"] = json!("global-string");
    assert_eq!(
        request(&req, "claude", Provider::Anthropic).unwrap()["provider_request"]["system"][0]["text"],
        "global-string"
    );
    req["messages"] = json!([{"role":"developer","content":"unsupported"}]);
    assert!(request(&req, "claude", Provider::Anthropic).is_err());
    req["messages"] = json!([{"role":"system","content":[{"type":"image","source":{}}]},{"role":"user","content":"x"}]);
    assert!(request(&req, "claude", Provider::Anthropic).is_err());
}

#[test]
fn ndjson_handles_fragmented_unicode_and_transport_statuses() {
    let wire = "\u{feff}{\"status\":{\"queued\":{\"position\":2}}}\r\n{\"status\":\"started\"}\n{\"event\":{\"text\":\"español 🦀\"}}\r\n{\"status\":\"stream_ended\"}";
    let mut decoder = Decoder::new(true);
    let mut out = vec![];
    for byte in wire.as_bytes() {
        out.extend(decoder.push(&[*byte], false).unwrap());
    }
    out.extend(decoder.push(&[], true).unwrap());
    assert_eq!(out, [json!({"text":"español 🦀"})]);
    assert!(decoder.push(b"{}\n", false).is_err());
    assert_eq!(
        Decoder::new(false)
            .push(b"{\"type\":\"native\"}", true)
            .unwrap(),
        [json!({"type":"native"})]
    );
}

#[test]
fn ndjson_detects_failure_truncation_and_oversized_lines() {
    assert!(
        Decoder::new(true)
            .push(b"{\"event\":{}}\n", true)
            .unwrap_err()
            .to_string()
            .contains("stream_ended")
    );
    let failed =
        json!({"status":{"failed":{"code":"overloaded","message":"busy","retry_after":5}}})
            .to_string();
    let error = Decoder::new(true)
        .push(failed.as_bytes(), true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("busy") && error.contains("overloaded"));
    assert!(
        Decoder::new(false)
            .push(&vec![b'x'; 4 * 1024 * 1024 + 1], false)
            .is_err()
    );
    assert!(
        Decoder::new(true)
            .push(b"{\"unexpected\":1}\n", false)
            .is_err()
    );
}

#[test]
fn anthropic_preserves_thinking_tools_cache_and_public_model() {
    let mut stream = Stream::new(Provider::Anthropic, "zed/public", &input()).unwrap();
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_a","type":"message","model":"native","role":"assistant","content":[],"usage":{"input_tokens":7,"output_tokens":0,"cache_read_input_tokens":11}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"razón"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tool_1","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"hola\"}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":9}}),
        json!({"type":"message_stop"}),
    ];
    let mut collector = Collector::default();
    assert!(stream.start().is_empty());
    for raw in &events {
        for event in stream.accept(raw).unwrap() {
            assert_ne!(
                event["type"], "message_stop",
                "stop must wait for transport completion"
            );
            collector.accept(&event).unwrap();
        }
    }
    for event in stream.finish().unwrap() {
        collector.accept(&event).unwrap();
    }
    let message = collector.finish().unwrap();
    assert_eq!(message["model"], "zed/public");
    assert_eq!(
        message["content"][0],
        json!({"type":"thinking","thinking":"razón","signature":"opaque"})
    );
    assert_eq!(message["content"][1]["input"], json!({"q":"hola"}));
    assert_eq!(
        message["usage"],
        json!({"input_tokens":7,"output_tokens":9,"cache_read_input_tokens":11})
    );
    assert_eq!(message["stop_reason"], "tool_use");
    let mut truncated = Stream::new(Provider::Anthropic, "zed/public", &input()).unwrap();
    truncated.accept(&events[0]).unwrap();
    assert!(truncated.finish().is_err());
}

#[test]
fn openai_and_xai_are_exposed_as_anthropic_messages() {
    for provider in [Provider::OpenAi, Provider::XAi] {
        let mut stream = Stream::new(provider, "zed/public", &input()).unwrap();
        let mut collector = Collector::default();
        for event in stream.start() {
            collector.accept(&event).unwrap();
        }
        for raw in [
            json!({"type":"response.output_text.delta","delta":"hola"}),
            json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hola"}]}],"usage":{"input_tokens":7,"output_tokens":2}}}),
        ] {
            for event in stream.accept(&raw).unwrap() {
                collector.accept(&event).unwrap();
            }
        }
        for event in stream.finish().unwrap() {
            collector.accept(&event).unwrap();
        }
        let response = collector.finish().unwrap();
        assert_eq!(response["type"], "message");
        assert_eq!(response["model"], "zed/public");
        assert_eq!(response["content"][0]["text"], "hola");
        assert_eq!(response["stop_reason"], "end_turn");
    }
}

#[test]
fn google_roundtrip_preserves_native_call_identity_and_thought_signatures() {
    let mut stream = Stream::new(Provider::Google, "zed/public", &input()).unwrap();
    let native_parts = json!([
        {"text":"razón","thought":true,"thoughtSignature":"thinking-signature"},
        {"functionCall":{"id":"call_native","name":"lookup","args":{"q":"hola"}},"thoughtSignature":"call-signature"}
    ]);
    let raw = json!({"candidates":[{"index":0,"content":{"parts":native_parts},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":20,"cachedContentTokenCount":5,"candidatesTokenCount":3,"thoughtsTokenCount":7}});
    let mut collector = Collector::default();
    for event in stream
        .start()
        .into_iter()
        .chain(stream.accept(&raw).unwrap())
        .chain(stream.finish().unwrap())
    {
        collector.accept(&event).unwrap();
    }
    let response = collector.finish().unwrap();
    assert_eq!(response["stop_reason"], "tool_use");
    assert_eq!(
        response["usage"],
        json!({"input_tokens":15,"cache_read_input_tokens":5,"output_tokens":10})
    );
    let mut req = input();
    req["messages"] = json!([
        {"role":"assistant","content":response["content"]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_native","content":"resultado"}]}
    ]);
    let body = request(&req, "gemini", Provider::Google).unwrap();
    assert_eq!(
        body["provider_request"]["contents"][0]["parts"],
        native_parts
    );
    assert_eq!(
        body["provider_request"]["contents"][1]["parts"][0]["functionResponse"]["id"],
        "call_native"
    );
    assert_eq!(
        body["provider_request"]["contents"][1]["parts"][0]["functionResponse"]["name"],
        "lookup"
    );
    let mut truncated = Stream::new(Provider::Google, "zed/public", &input()).unwrap();
    assert!(truncated.finish().is_err());
    req["messages"] = json!([{"role":"user","content":[{"type":"video","source":{}}]}]);
    assert!(request(&req, "gemini", Provider::Google).is_err());
}

#[test]
fn google_rejects_unrepresentable_controls_and_preserves_standalone_signatures() {
    for option in [
        json!({"output_config":{"format":{"type":"json_schema","schema":{}}}}),
        json!({"thinking":{"type":"adaptive"}}),
        json!({"tools":[{"name":"lookup","input_schema":{},"strict":true}]}),
        json!({"tool_choice":{"type":"auto","disable_parallel_tool_use":true}}),
    ] {
        let mut req = input();
        req.as_object_mut()
            .unwrap()
            .extend(option.as_object().unwrap().clone());
        assert!(request(&req, "gemini", Provider::Google).is_err());
    }
    let mut stream = Stream::new(Provider::Google, "zed/public", &input()).unwrap();
    let parts = json!([{"text":"answer"},{"thoughtSignature":"opaque-only"}]);
    let events = stream
        .accept(&json!({"candidates":[{"content":{"parts":parts},"finishReason":"STOP"}]}))
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "content_block_start")
            .count(),
        2
    );
    let capsule = &events
        .iter()
        .find(|e| e["content_block"]["type"] == "redacted_thinking")
        .unwrap()["content_block"];
    assert_eq!(
        translate::replay_state(capsule, "zed/public", "zed_google")
            .unwrap()
            .unwrap(),
        parts
    );
    let mut req = input();
    req["stop_sequences"] = json!(["END"]);
    assert!(
        warnings(&req, "gemini", Provider::Google)
            .contains(&"google_finish_reason_does_not_identify_matched_stop_sequence")
    );
}
