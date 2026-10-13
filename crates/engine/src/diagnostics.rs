//! Content-free observations shared by every transport adapter.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TracePolicy {
    pub enabled: bool,
    /// Explicit consent expires; full capture cannot silently stay enabled.
    pub full_until: Option<u64>,
    pub retention_hours: u64,
}
impl TracePolicy {
    pub fn validate(&self, now: u64) -> anyhow::Result<()> {
        anyhow::ensure!(self.retention_hours <= 168, "Retención máxima: 168 horas");
        anyhow::ensure!(
            !self.enabled || self.retention_hours > 0,
            "Indica la retención de las trazas"
        );
        anyhow::ensure!(
            self.full_until.is_none_or(|until| until <= now + 86400),
            "Consentimiento de captura completa: máximo 24 horas"
        );
        Ok(())
    }
}

/// Only controlled labels and counters; arbitrary JSON keys/values never escape.
pub fn observe(value: &Value) -> Value {
    fn visit(v: &Value, counts: &mut [u64; 6]) {
        match v {
            Value::Object(o) => {
                match o.get("type").and_then(Value::as_str) {
                    Some("tool_use" | "function_call") => counts[0] += 1,
                    Some("tool_result" | "function_call_output") => counts[1] += 1,
                    Some("thinking" | "redacted_thinking" | "reasoning") => counts[2] += 1,
                    Some("image" | "image_url" | "input_image") => counts[3] += 1,
                    Some("document" | "file" | "input_file") => counts[4] += 1,
                    _ => {}
                }
                if let Some(tools) = o.get("tool_calls").and_then(Value::as_array) {
                    counts[0] += tools.len() as u64;
                }
                if o.get("role").and_then(Value::as_str) == Some("tool") {
                    counts[1] += 1;
                }
                if o.contains_key("reasoning_content") || o.contains_key("encrypted_content") {
                    counts[2] += 1;
                }
                if o.contains_key("functionCall") {
                    counts[0] += 1;
                }
                if o.contains_key("functionResponse") {
                    counts[1] += 1;
                }
                if o.get("thought") == Some(&Value::Bool(true))
                    || o.contains_key("thoughtSignature")
                {
                    counts[2] += 1;
                }
                if let Some(data) = o.get("inlineData") {
                    if data["mimeType"]
                        .as_str()
                        .is_some_and(|mime| mime.starts_with("image/"))
                    {
                        counts[3] += 1;
                    } else {
                        counts[4] += 1;
                    }
                }
                if let Some(s) = o.get("text").and_then(Value::as_str) {
                    counts[5] += s.chars().count() as u64;
                }
                for child in o.values() {
                    visit(child, counts);
                }
            }
            Value::Array(a) => {
                for child in a {
                    visit(child, counts);
                }
            }
            _ => {}
        }
    }
    let mut counts = [0; 6];
    visit(value, &mut counts);
    let event = value["type"].as_str().filter(|s| {
        matches!(
            *s,
            "message_start"
                | "message_delta"
                | "message_stop"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "error"
                | "ping"
                | "response.created"
                | "response.completed"
                | "response.failed"
                | "response.incomplete"
                | "response.output_item.added"
                | "response.output_item.done"
                | "response.output_text.delta"
                | "response.function_call_arguments.delta"
                | "response.reasoning_summary_text.delta"
        )
    });
    let delta = value["delta"]["type"].as_str().filter(|s| {
        matches!(
            *s,
            "text_delta" | "input_json_delta" | "thinking_delta" | "signature_delta"
        )
    });
    let mut usage = serde_json::Map::new();
    for key in [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        let tokens = value["usage"][key]
            .as_u64()
            .or_else(|| value["message"]["usage"][key].as_u64())
            .or_else(|| value["response"]["usage"][key].as_u64());
        if let Some(tokens) = tokens {
            usage.insert(key.into(), json!(tokens));
        }
    }
    for (source, dest) in [
        ("promptTokenCount", "input_tokens"),
        ("candidatesTokenCount", "output_tokens"),
        ("cachedContentTokenCount", "cache_read_input_tokens"),
        ("thoughtsTokenCount", "reasoning_tokens"),
    ] {
        if let Some(tokens) = value["usageMetadata"][source].as_u64() {
            usage.insert(dest.into(), json!(tokens));
        }
    }
    json!({"bytes":value.to_string().len(),"event":event,"delta":delta,"index":value["index"].as_u64(),"output_index":value["output_index"].as_u64(),"usage":usage,"tool_calls":counts[0],
        "tool_results":counts[1],"reasoning_blocks":counts[2],"images":counts[3],
        "files":counts[4],"text_characters":counts[5]})
}

/// Full capture still excludes credential fields. HTTP headers are never captured.
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    let key = k.to_ascii_lowercase();
                    let secret = [
                        "authorization",
                        "api_key",
                        "access_token",
                        "refresh_token",
                        "id_token",
                        "password",
                        "secret",
                        "x-api-key",
                    ]
                    .iter()
                    .any(|s| key.contains(s));
                    (
                        k.clone(),
                        if secret {
                            json!("[redacted]")
                        } else {
                            redact(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        _ => value.clone(),
    }
}

/// Conservative checks of completed JSON responses. Only proven losses are reported;
/// local stopping deliberately removes subsequent output. No content is returned.
pub fn compare_response(
    raw: &Value,
    converted: &Value,
    req: &Value,
    protocol: crate::Protocol,
) -> Value {
    if converted["stop_reason"] == "stop_sequence" {
        return json!({"confirmed_losses":[],"expected_changes":["local_stop_filtered_output"]});
    }
    if protocol == crate::Protocol::Messages {
        return json!({"confirmed_losses":[],"expected_changes":[]});
    }
    let source: Vec<&Value> = if protocol == crate::Protocol::Chat {
        raw["choices"][0]["message"]["tool_calls"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    } else {
        raw["output"]
            .as_array()
            .map(|a| a.iter().filter(|v| v["type"] == "function_call").collect())
            .unwrap_or_default()
    };
    let target: Vec<&Value> = converted["content"]
        .as_array()
        .map(|a| a.iter().filter(|v| v["type"] == "tool_use").collect())
        .unwrap_or_default();
    let names = crate::translate::tool_names(req);
    let mut losses = Vec::new();
    if source.len() != target.len() {
        losses.push("tool_call_count_changed");
    }
    for call in source {
        let (id, name, arguments) = if protocol == crate::Protocol::Chat {
            (
                &call["id"],
                &call["function"]["name"],
                &call["function"]["arguments"],
            )
        } else {
            (
                if call["call_id"].as_str().is_some_and(|s| !s.is_empty()) {
                    &call["call_id"]
                } else {
                    &call["id"]
                },
                &call["name"],
                &call["arguments"],
            )
        };
        let name = name.as_str().unwrap_or_default();
        let restored = names.get(name).map(String::as_str).unwrap_or(name);
        if let Some(converted) = target.iter().find(|c| c["id"] == *id) {
            if converted["name"] != restored {
                losses.push("tool_name_changed");
            }
            if crate::translate::parse_arguments(arguments)
                .is_ok_and(|args| converted["input"] != args)
            {
                losses.push("tool_arguments_changed");
            }
        } else {
            losses.push("tool_call_id_lost");
        }
    }
    if protocol == crate::Protocol::Chat
        && let Some(text) = raw["choices"][0]["message"]["content"].as_str()
    {
        let output: String = converted["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect();
        if !output.contains(text) {
            losses.push("response_text_lost");
        }
    }
    losses.sort_unstable();
    losses.dedup();
    json!({"confirmed_losses":losses,"expected_changes":[]})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn google_observations_count_native_tools_thoughts_media_and_tokens_without_content() {
        let raw = json!({"contents":[{"parts":[{"functionCall":{"name":"private-tool","args":{"q":"private-input"}}},{"functionResponse":{"name":"private-tool","response":{"text":"private-result"}}},{"thought":true,"text":"private-thought","thoughtSignature":"private-signature"},{"inlineData":{"mimeType":"image/png","data":"private-image"}},{"inlineData":{"mimeType":"application/pdf","data":"private-file"}}]}],"usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":3,"cachedContentTokenCount":5,"thoughtsTokenCount":7}});
        let observation = observe(&raw);
        assert_eq!(observation["tool_calls"], 1);
        assert_eq!(observation["tool_results"], 1);
        assert_eq!(observation["reasoning_blocks"], 1);
        assert_eq!(observation["images"], 1);
        assert_eq!(observation["files"], 1);
        assert_eq!(observation["usage"]["reasoning_tokens"], 7);
        assert!(!observation.to_string().contains("private"));
    }
    #[test]
    fn metadata_cannot_leak_user_values_or_custom_event_types() {
        let value = json!({"type":"secret-event", "prompt":"secret-prompt", "tool_calls":[{"id":"secret-id"}], "reasoning_content":"private-thought"});
        let observed = observe(&value).to_string();
        assert!(!observed.contains("secret"));
        assert!(!observed.contains("private"));
        assert_eq!(observe(&value)["tool_calls"], 1);
    }
    #[test]
    fn distinguishes_actual_tool_loss_from_expected_stop_filtering() {
        let raw = json!({"choices":[{"message":{"tool_calls":[{"id":"private-id","function":{"name":"read","arguments":"{}"}}]}}]});
        let converted = json!({"content":[],"stop_reason":"end_turn"});
        let report = compare_response(&raw, &converted, &json!({}), crate::Protocol::Chat);
        assert!(report["confirmed_losses"].as_array().unwrap().len() >= 2);
        assert!(!report.to_string().contains("private-id"));
        let stopped = json!({"content":[],"stop_reason":"stop_sequence"});
        assert_eq!(
            compare_response(&raw, &stopped, &json!({}), crate::Protocol::Chat)["confirmed_losses"],
            json!([])
        );
    }
}
