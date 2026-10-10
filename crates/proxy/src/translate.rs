use crate::provider::Protocol;
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::borrow::Cow;

pub fn text(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.into();
    }
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn blocks(value: &Value) -> Cow<'_, [Value]> {
    if let Some(s) = value.as_str() {
        Cow::Owned(vec![json!({"type":"text","text":s})])
    } else {
        Cow::Borrowed(value.as_array().map(Vec::as_slice).unwrap_or_default())
    }
}

fn image(block: &Value) -> Result<String> {
    let s = &block["source"];
    match s["type"].as_str() {
        Some("url") => {
            ensure!(
                s["url"].as_str().is_some_and(|s| !s.is_empty()),
                "Imagen sin URL"
            );
            Ok(s["url"].as_str().unwrap().into())
        }
        Some("base64") => {
            let media = s["media_type"].as_str().unwrap_or_default();
            let data = s["data"].as_str().unwrap_or_default();
            ensure!(
                !media.is_empty() && !data.is_empty(),
                "Imagen base64 requiere media_type y data"
            );
            Ok(format!("data:{media};base64,{data}"))
        }
        _ => bail!("Tipo de imagen no soportado"),
    }
}

pub fn request(req: &Value, model: &str, protocol: Protocol) -> Result<Value> {
    if protocol == Protocol::Messages {
        let mut body = req.clone();
        body["model"] = json!(model);
        return Ok(body);
    }
    let responses = protocol == Protocol::Responses;
    let mut input = Vec::new();
    let system = text(&req["system"]);
    if !system.is_empty() {
        input.push(if responses {
            json!({"role":"system","content":[{"type":"input_text","text":system}]})
        } else {
            json!({"role":"system","content":system})
        });
    }
    for message in req["messages"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("messages debe ser una lista"))?
    {
        let role = message["role"].as_str().unwrap_or_default();
        let content = blocks(&message["content"]);
        match role {
            "system" => {
                let t = text(&message["content"]);
                input.push(if responses {
                    json!({"role":role,"content":[{"type":"input_text","text":t}]})
                } else {
                    json!({"role":role,"content":t})
                });
            }
            "user" => {
                let mut parts = Vec::new();
                for b in content.iter() {
                    match b["type"].as_str().unwrap_or_default() {
                        "text" => parts.push(json!({"type":if responses {"input_text"} else {"text"},"text":b["text"]})),
                        "image" => {
                            let url = image(b)?;
                            parts.push(if responses { json!({"type":"input_image","image_url":url}) } else { json!({"type":"image_url","image_url":{"url":url}}) });
                        },
                        "tool_result" => {
                            if !parts.is_empty() { input.push(json!({"role":"user","content":std::mem::take(&mut parts)})); }
                            let output = text(&b["content"]);
                            input.push(if responses { json!({"type":"function_call_output","call_id":b["tool_use_id"],"output":output}) }
                                else { json!({"role":"tool","tool_call_id":b["tool_use_id"],"content":output}) });
                        },
                        "thinking" | "redacted_thinking" => {},
                        other => bail!("Bloque de usuario no soportado: {other}"),
                    }
                }
                if !parts.is_empty() {
                    let c = if !responses && parts.len() == 1 && parts[0]["type"] == "text" {
                        parts[0]["text"].clone()
                    } else {
                        json!(parts)
                    };
                    input.push(json!({"role":"user","content":c}));
                } else if content.is_empty() {
                    input.push(
                        json!({"role":"user","content":if responses {json!([])} else {json!("")}}),
                    );
                }
            }
            "assistant" => {
                let mut t = String::new();
                let mut calls = Vec::new();
                for b in content.iter() {
                    match b["type"].as_str() {
                        Some("text") => t.push_str(b["text"].as_str().unwrap_or_default()),
                        Some("image") => t.push_str("[imagen]"),
                        Some("tool_use") => {
                            let arguments = b.get("input").unwrap_or(&json!({})).to_string();
                            calls.push(if responses { json!({"type":"function_call","call_id":b["id"],"name":b["name"],"arguments":arguments}) }
                                else { json!({"id":b["id"],"type":"function","function":{"name":b["name"],"arguments":arguments}}) });
                        }
                        _ => {}
                    }
                }
                if responses {
                    if !t.is_empty() || calls.is_empty() {
                        input.push(
                            json!({"role":"assistant","content":[{"type":"output_text","text":t}]}),
                        );
                    }
                    input.extend(calls);
                } else {
                    let mut m = json!({"role":"assistant","content":if t.is_empty() && !calls.is_empty() { Value::Null } else {json!(t)}});
                    if !calls.is_empty() {
                        m["tool_calls"] = json!(calls);
                    }
                    input.push(m);
                }
            }
            _ => bail!("Rol no soportado: {role}"),
        }
    }
    let stream = req["stream"].as_bool().unwrap_or(false);
    let mut out = json!({"model":model,"stream":stream});
    out[if responses { "input" } else { "messages" }] = json!(input);
    if responses {
        out["store"] = json!(false);
    } else if stream {
        out["stream_options"] = json!({"include_usage":true});
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = req.get(key) {
            out[key] = v.clone();
        }
    }
    if req["max_tokens"].as_u64().unwrap_or(0) > 0 {
        out[if responses {
            "max_output_tokens"
        } else {
            "max_tokens"
        }] = req["max_tokens"].clone();
    }
    if !responses && req.get("stop_sequences").is_some() {
        out["stop"] = req["stop_sequences"].clone();
    }
    if let Some(tools) = req["tools"].as_array().filter(|t| !t.is_empty()) {
        out["tools"] = json!(tools.iter().map(|t| {
            let mut f = json!({"name":t["name"],"description":t.get("description").cloned().unwrap_or(json!("")),"parameters":t.get("input_schema").cloned().unwrap_or(json!({"type":"object"}))});
            if responses { f["type"] = json!("function"); f } else { json!({"type":"function","function":f}) }
        }).collect::<Vec<_>>());
    }
    if let Some(tc) = req.get("tool_choice") {
        out["tool_choice"] = match tc["type"].as_str().unwrap_or("auto") {
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => {
                if responses {
                    json!({"type":"function","name":tc["name"]})
                } else {
                    json!({"type":"function","function":{"name":tc["name"]}})
                }
            }
            _ => json!("auto"),
        };
        if let Some(disable) = tc["disable_parallel_tool_use"].as_bool() {
            out["parallel_tool_calls"] = json!(!disable);
        }
    }
    Ok(out)
}

pub fn stop(reason: &str) -> &'static str {
    match reason {
        "tool_calls" => "tool_use",
        "length" | "max_output_tokens" => "max_tokens",
        _ => "end_turn",
    }
}

pub fn response(value: &Value, model: &str, protocol: Protocol) -> Result<Value> {
    if protocol == Protocol::Messages {
        return Ok(value.clone());
    }
    let responses = protocol == Protocol::Responses;
    let mut content = Vec::new();
    let mut reason = "end_turn";
    let (input_tokens, output_tokens);
    if responses {
        ensure!(value["output"].is_array(), "Respuesta Responses sin output");
        ensure!(
            value["status"] != "failed",
            "Responses falló: {}",
            value["error"]
        );
        for item in value["output"].as_array().unwrap() {
            match item["type"].as_str() {
                Some("message") => {
                    if let Some(parts) = item["content"].as_array() {
                        for p in parts {
                            if p["type"] == "output_text" {
                                content.push(json!({"type":"text","text":p["text"]}));
                            }
                        }
                    }
                }
                Some("function_call") => {
                    reason = "tool_use";
                    content.push(json!({"type":"tool_use","id":item.get("call_id").unwrap_or(&item["id"]),"name":item["name"],"input":parse_arguments(&item["arguments"])?}));
                }
                _ => {}
            }
        }
        if reason != "tool_use" && value["status"] == "incomplete" {
            reason = stop(
                value["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or_default(),
            );
        }
        input_tokens = value["usage"]["input_tokens"].as_u64().unwrap_or(0);
        output_tokens = value["usage"]["output_tokens"].as_u64().unwrap_or(0);
    } else {
        let choice = value["choices"]
            .get(0)
            .ok_or_else(|| anyhow::anyhow!("Respuesta Chat sin choices"))?;
        let t = text(&choice["message"]["content"]);
        if !t.is_empty() {
            content.push(json!({"type":"text","text":t}));
        }
        if let Some(calls) = choice["message"]["tool_calls"].as_array() {
            for call in calls {
                content.push(json!({"type":"tool_use","id":call["id"],"name":call["function"]["name"],"input":parse_arguments(&call["function"]["arguments"])?}));
            }
        }
        reason = stop(choice["finish_reason"].as_str().unwrap_or_default());
        input_tokens = value["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        output_tokens = value["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    }
    if content.is_empty() {
        content.push(json!({"type":"text","text":""}));
    }
    Ok(
        json!({"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":model,"content":content,"stop_reason":reason,"stop_sequence":null,"usage":{"input_tokens":input_tokens,"output_tokens":output_tokens}}),
    )
}

fn parse_arguments(value: &Value) -> Result<Value> {
    let s = value.as_str().unwrap_or("{}");
    let parsed: Value = serde_json::from_str(if s.is_empty() { "{}" } else { s })?;
    ensure!(
        parsed.is_object(),
        "Los argumentos de herramienta deben ser un objeto"
    );
    Ok(parsed)
}
