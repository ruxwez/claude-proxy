use crate::provider::Protocol;
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::borrow::Cow;

#[path = "translate/content.rs"]
mod content;
#[path = "translate/stop.rs"]
mod stop;
pub(crate) use stop::StopFilter;

// A proxy-owned transport capsule, NOT an Anthropic signature or ciphertext.
// The client keeps it in its own history, so there is no shared session cache.
const STATE_PREFIX: &str = "claude-proxy-state-v1:";
// Leave room for base64 and the SSE wrapper within the 4 MiB frame limit.
const MAX_STATE_BYTES: usize = 3 * 1024 * 1024 - 4096;

pub(crate) fn state_block(model: &str, kind: &str, value: &Value) -> Result<Value> {
    let bytes = serde_json::to_vec(&json!({"model":model,"kind":kind,"value":value}))?;
    ensure!(
        bytes.len() <= MAX_STATE_BYTES,
        "Estado de razonamiento demasiado grande"
    );
    Ok(
        json!({"type":"redacted_thinking","data":format!("{STATE_PREFIX}{}", STANDARD.encode(bytes))}),
    )
}

fn is_proxy_state(block: &Value) -> bool {
    block["type"] == "redacted_thinking"
        && block["data"]
            .as_str()
            .is_some_and(|s| s.starts_with(STATE_PREFIX))
}

fn replay_state(block: &Value, model: &str, kind: &str) -> Result<Option<Value>> {
    if !is_proxy_state(block) {
        return Ok(None);
    }
    let data = block["data"]
        .as_str()
        .unwrap()
        .strip_prefix(STATE_PREFIX)
        .unwrap();
    ensure!(
        data.len() <= MAX_STATE_BYTES.div_ceil(3) * 4,
        "Estado de razonamiento demasiado grande"
    );
    let bytes = STANDARD.decode(data)?;
    ensure!(
        bytes.len() <= MAX_STATE_BYTES,
        "Estado de razonamiento demasiado grande"
    );
    let capsule: Value = serde_json::from_slice(&bytes)?;
    if capsule["model"] != model || capsule["kind"] != kind {
        return Ok(None);
    }
    let value = capsule["value"].clone();
    match kind {
        "responses" => ensure!(
            value["type"] == "reasoning"
                && value["encrypted_content"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
            "Estado Responses inválido"
        ),
        "chat_tools" => ensure!(value.is_object(), "Metadatos de herramientas inválidos"),
        "deepseek" => ensure!(value.is_string(), "Estado DeepSeek inválido"),
        "responses_message" => ensure!(
            value["type"] == "message"
                && value["role"] == "assistant"
                && value["content"].as_array().is_some_and(|a| a
                    .iter()
                    .all(|p| matches!(p["type"].as_str(), Some("output_text" | "refusal")))),
            "Estado de mensaje Responses inválido"
        ),
        "chat_reasoning" => ensure!(
            value
                .as_object()
                .is_some_and(|o| o.iter().all(|(k, v)| if k == "reasoning_details" {
                    v.as_array().is_some_and(|a| a.iter().all(Value::is_object))
                } else {
                    matches!(k.as_str(), "reasoning_content" | "reasoning") && v.is_string()
                })),
            "Estado Chat de razonamiento inválido"
        ),
        _ => bail!("Tipo de estado desconocido"),
    }
    Ok(Some(value))
}

pub(crate) fn deepseek(model: &str) -> bool {
    let model = model.rsplit('/').next().unwrap_or(model);
    model == "deepseek-flash"
        || model.starts_with("deepseek-v4-")
        || model.starts_with("deepseek-v4.")
}

pub(crate) fn responses_state(model: &str, item: &Value) -> Result<Option<Value>> {
    if item["type"] != "reasoning"
        || !item["encrypted_content"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    {
        return Ok(None);
    }
    // Preserve only the replayable API fields, never a response wrapper or a
    // provider-specific instruction. The encrypted value remains opaque.
    let mut state = json!({"type":"reasoning","summary":item.get("summary").cloned().unwrap_or(json!([])),"encrypted_content":item["encrypted_content"]});
    for field in ["id", "status", "content"] {
        if let Some(value) = item.get(field) {
            state[field] = value.clone();
        }
    }
    Ok(Some(state_block(model, "responses", &state)?))
}

pub(crate) fn responses_message_state(model: &str, item: &Value) -> Result<Option<Value>> {
    if item.get("id").is_none() && item.get("phase").is_none() {
        return Ok(None);
    }
    let mut message = json!({"type":"message","role":"assistant","content":item["content"]});
    for field in ["id", "status", "phase"] {
        if let Some(value) = item.get(field) {
            message[field] = value.clone();
        }
    }
    Ok(Some(state_block(model, "responses_message", &message)?))
}

fn assistant_message(text: &str, originals: &[Value]) -> Value {
    originals
        .iter()
        .find(|message| {
            message["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .filter_map(|part| output_part(part).ok())
                    .map(|(p, _)| p["text"].as_str().unwrap_or_default().to_string())
                    .collect::<String>()
                    == text
            })
        })
        .cloned()
        .unwrap_or_else(
            || json!({"role":"assistant","content":[{"type":"output_text","text":text}]}),
        )
}

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

// A small, conservative translation policy, not model discovery. Unknown IDs
// (including custom aliases and non-OpenAI providers) keep the generic mapping.
// Exact IDs and dated snapshots are accepted; e.g. gpt-5-chat-latest is not gpt-5.
fn reasoning_model(model: &str) -> Option<(&'static str, bool, bool)> {
    let matches = |id: &str| {
        model == id
            || model.strip_prefix(id).is_some_and(|suffix| {
                let date = suffix.strip_prefix('-').unwrap_or_default();
                date.len() == 10
                    && date.bytes().enumerate().all(|(i, b)| {
                        if i == 4 || i == 7 {
                            b == b'-'
                        } else {
                            b.is_ascii_digit()
                        }
                    })
            })
    };
    // (highest effort, supports none, requires Responses for reasoning + tools)
    for (ids, policy) in [
        (
            &["o1", "o3", "o3-mini", "o4-mini"][..],
            ("high", false, false),
        ),
        (
            &["gpt-5", "gpt-5-mini", "gpt-5-nano"][..],
            ("high", false, false),
        ),
        (&["gpt-5.1"][..], ("high", true, false)),
        (&["gpt-5.2"][..], ("xhigh", true, false)),
        (
            &["gpt-5.4", "gpt-5.4-mini", "gpt-5.4-nano", "gpt-5.5"][..],
            ("xhigh", true, true),
        ),
        (
            &[
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-6-sol",
                "gpt-6-luna",
            ][..],
            ("max", true, true),
        ),
        (&["gpt-6-astra", "gpt-6.1-sol"][..], ("max", false, true)),
    ] {
        if ids.iter().any(|id| matches(id)) {
            return Some(policy);
        }
    }
    None
}

fn reasoning(req: &Value, model: &str, protocol: Protocol, out: &mut Value) -> Result<()> {
    if deepseek(model) && protocol == Protocol::Chat {
        if let Some(mode) = req["thinking"]["type"].as_str() {
            out["thinking"] = json!({"type":if mode == "disabled" {"disabled"} else {"enabled"}});
        }
        if req["thinking"]["type"] != "disabled" {
            let effort = req["output_config"]["effort"]
                .as_str()
                .or_else(|| req["effort"].as_str());
            if let Some(effort) = effort {
                out["reasoning_effort"] = json!(match effort {
                    "low" | "minimal" => "low",
                    "max" | "ultra" => "max",
                    _ => "high",
                });
            }
            out.as_object_mut().unwrap().remove("temperature");
            ensure!(
                !matches!(req["tool_choice"]["type"].as_str(), Some("any" | "tool")),
                "DeepSeek no admite herramientas forzadas en modo de pensamiento"
            );
        }
        return Ok(());
    }
    let Some((highest, supports_none, responses_tools)) = reasoning_model(model) else {
        return Ok(());
    };
    let effort = if req["thinking"]["type"] == "disabled" {
        supports_none.then_some("none")
    } else {
        // budget_tokens is a hard Anthropic budget, not an effort level. An
        // explicit enabled/adaptive mode without effort uses medium instead;
        // no numeric budget conversion is implied.
        match req["output_config"]["effort"]
            .as_str()
            .or_else(|| req["effort"].as_str())
        {
            Some("low") => Some("low"),
            Some("medium") => Some("medium"),
            Some("high") => Some("high"),
            Some("xhigh") => Some(if highest == "high" { "high" } else { "xhigh" }),
            Some("max") => Some(highest),
            _ if matches!(
                req["thinking"]["type"].as_str(),
                Some("enabled" | "adaptive")
            ) =>
            {
                Some("medium")
            }
            _ => None,
        }
    };
    let has_tools = req["tools"].as_array().is_some_and(|t| !t.is_empty());
    ensure!(
        protocol != Protocol::Chat
            || !responses_tools
            || !has_tools
            || (supports_none && effort == Some("none")),
        "{model} requiere el protocolo Responses para herramientas con razonamiento; selecciona Responses para este modelo en la UI"
    );
    if let Some(effort) = effort {
        if protocol == Protocol::Responses {
            out["reasoning"] = json!({"effort":effort});
        } else {
            out["reasoning_effort"] = json!(effort);
        }
    }
    if effort != Some("none") {
        out.as_object_mut().unwrap().remove("temperature");
        out.as_object_mut().unwrap().remove("top_p");
    }
    out.as_object_mut().unwrap().remove("stop");
    Ok(())
}

pub fn request(req: &Value, model: &str, protocol: Protocol) -> Result<Value> {
    if protocol == Protocol::Messages {
        let mut body = req.clone();
        body["model"] = json!(model);
        // A model/protocol switch must not send our transport capsules to an
        // Anthropic-compatible provider as if they were native redactions.
        if let Some(messages) = body["messages"].as_array_mut() {
            for message in messages.iter_mut() {
                if let Some(content) = message["content"].as_array_mut() {
                    content.retain(|block| !is_proxy_state(block));
                }
            }
            messages.retain(|m| {
                m["role"] != "assistant" || m["content"].as_array().is_none_or(|a| !a.is_empty())
            });
        }
        return Ok(body);
    }
    validate_portable(req)?;
    StopFilter::new(req)?;
    let public_model = req["model"].as_str().unwrap_or(model);
    let responses = protocol == Protocol::Responses;
    let mut input = Vec::new();
    let system = text(&req["system"]);
    if !system.is_empty() {
        input.push(if req["system"].as_array().is_some_and(|parts| parts.iter().any(|p| p.get("cache_control").is_some())) {
            json!({"role":"system","content":content::input_parts(&req["system"], responses, &req["tools"])?})
        } else if responses {
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
        if let Some(config) = message.get("output_config") {
            ensure!(
                responses,
                "Los cambios de esfuerzo dentro del historial requieren Responses o Messages nativo"
            );
            ensure!(
                config
                    .as_object()
                    .is_some_and(|o| o.len() == 1 && o.contains_key("effort")),
                "Actualización de configuración no portable"
            );
            let mut update = json!({});
            reasoning(
                &json!({"output_config":config}),
                model,
                protocol,
                &mut update,
            )?;
            ensure!(
                update["reasoning"].is_object(),
                "Actualización de esfuerzo no reconocida para {model}"
            );
            input.push(json!({"type":"configuration_update","reasoning":update["reasoning"]}));
        }
        let content = blocks(&message["content"]);
        match role {
            "system" => {
                let t = text(&message["content"]);
                if t.is_empty() && message.get("output_config").is_some() {
                    continue;
                }
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
                        "text" | "image" | "document" | "search_result" | "tool_reference" => {
                            parts.extend(content::input_parts(
                                &json!([b]),
                                responses,
                                &req["tools"],
                            )?);
                        }
                        "tool_result" => {
                            let (result, attachments) =
                                content::tool_result(b, responses, &req["tools"])?;
                            input.push(result);
                            parts.extend(attachments);
                        }
                        "thinking" | "redacted_thinking" => {}
                        other => bail!("Bloque de usuario no soportado: {other}"),
                    }
                }
                if !parts.is_empty() {
                    let c = if !responses
                        && parts.len() == 1
                        && parts[0]["type"] == "text"
                        && parts[0].get("prompt_cache_breakpoint").is_none()
                    {
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
                let mut originals = Vec::new();
                if responses {
                    for block in content.iter() {
                        if let Some(message) =
                            replay_state(block, public_model, "responses_message")?
                        {
                            originals.push(message);
                        }
                    }
                }
                let mut t = String::new();
                let mut calls = Vec::new();
                let mut thought = None;
                let mut chat_reasoning = None;
                let mut tool_metadata = serde_json::Map::new();
                for b in content.iter() {
                    match b["type"].as_str() {
                        Some("text") => t.push_str(&content::cited_text(b)),
                        Some("image" | "document") => bail!(
                            "Contenido multimedia del asistente no portable; usa Messages nativo"
                        ),
                        Some("tool_use") => {
                            ensure!(
                                b["id"].as_str().is_some_and(|s| !s.is_empty())
                                    && b["name"].as_str().is_some_and(|s| !s.is_empty())
                                    && b["input"].is_object(),
                                "tool_use requiere id, name e input objeto"
                            );
                            if responses && !t.is_empty() {
                                input.push(assistant_message(&std::mem::take(&mut t), &originals));
                            }
                            let arguments = b.get("input").unwrap_or(&json!({})).to_string();
                            let name = content::tool_name(b["name"].as_str().unwrap());
                            if responses {
                                input.push(json!({"type":"function_call","call_id":b["id"],"name":name,"arguments":arguments}));
                            } else {
                                calls.push(json!({"id":b["id"],"type":"function","function":{"name":name,"arguments":arguments}}));
                            }
                        }
                        Some("redacted_thinking") => {
                            if !responses
                                && let Some(state) =
                                    replay_state(b, public_model, "chat_reasoning")?
                            {
                                chat_reasoning = Some(state);
                            }
                            if !responses
                                && let Some(state) = replay_state(b, public_model, "chat_tools")?
                            {
                                tool_metadata.extend(state.as_object().unwrap().clone());
                            }
                            if responses {
                                if replay_state(b, public_model, "responses_message")?.is_some()
                                    && !t.is_empty()
                                {
                                    input.push(assistant_message(
                                        &std::mem::take(&mut t),
                                        &originals,
                                    ));
                                }
                                if let Some(state) = replay_state(b, public_model, "responses")? {
                                    if !t.is_empty() {
                                        input.push(assistant_message(
                                            &std::mem::take(&mut t),
                                            &originals,
                                        ));
                                    }
                                    input.push(state);
                                }
                            } else if deepseek(model)
                                && let Some(state) = replay_state(b, public_model, "deepseek")?
                            {
                                thought = Some(state);
                            }
                        }
                        Some("thinking") => {}
                        other => bail!(
                            "Bloque de asistente no portable: {}; usa Messages nativo",
                            other.unwrap_or("sin tipo")
                        ),
                    }
                }
                if responses {
                    if !t.is_empty() || content.is_empty() {
                        input.push(assistant_message(&t, &originals));
                    }
                } else {
                    if t.is_empty()
                        && calls.is_empty()
                        && thought.is_none()
                        && chat_reasoning.is_none()
                        && !content.is_empty()
                    {
                        continue;
                    }
                    let mut m = json!({"role":"assistant","content":if t.is_empty() && !calls.is_empty() { Value::Null } else {json!(t)}});
                    for call in &mut calls {
                        if let Some(metadata) =
                            call["id"].as_str().and_then(|id| tool_metadata.get(id))
                        {
                            ensure!(metadata.is_object(), "extra_content debe ser un objeto");
                            call["extra_content"] = metadata.clone();
                        }
                    }
                    if !calls.is_empty() {
                        m["tool_calls"] = json!(calls);
                    }
                    if let Some(state) = chat_reasoning {
                        m.as_object_mut()
                            .unwrap()
                            .extend(state.as_object().unwrap().clone());
                    }
                    if deepseek(model) {
                        // DeepSeek requires non-null assistant content and all
                        // reasoning_content values when tools are in history.
                        if m["content"].is_null() {
                            m["content"] = json!("");
                        }
                        m["reasoning_content"] = thought.unwrap_or(json!(""));
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
        // This is a Responses transport capability, independent of whether we
        // recognize the model's reasoning-effort controls.
        out["include"] = json!(["reasoning.encrypted_content"]);
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
        } else if reasoning_model(model).is_some() {
            "max_completion_tokens"
        } else {
            "max_tokens"
        }] = req["max_tokens"].clone();
    }
    if !responses && req.get("stop_sequences").is_some() {
        out["stop"] = req["stop_sequences"].clone();
    }
    if let Some(tools) = req["tools"].as_array().filter(|t| !t.is_empty()) {
        out["tools"] = content::tools(&json!(tools), responses)?;
    }
    if let Some(tc) = req.get("tool_choice") {
        out["tool_choice"] = match tc["type"].as_str().unwrap_or("auto") {
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => {
                let name = content::tool_name(
                    tc["name"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("tool_choice sin nombre"))?,
                );
                if responses {
                    json!({"type":"function","name":name})
                } else {
                    json!({"type":"function","function":{"name":name}})
                }
            }
            "auto" => json!("auto"),
            other => bail!("tool_choice non portable: {other}"),
        };
        if let Some(disable) = tc["disable_parallel_tool_use"].as_bool() {
            out["parallel_tool_calls"] = json!(!disable);
        }
    }
    if let Some(format) = req["output_config"]
        .get("format")
        .or_else(|| req.get("output_format"))
    {
        ensure!(
            format["type"] == "json_schema",
            "Formato de salida no portable"
        );
        ensure!(format["schema"].is_object(), "Formato JSON sin schema");
        let schema =
            json!({"type":"json_schema","name":"response","schema":format["schema"],"strict":true});
        if responses {
            out["text"] = json!({"format":schema});
        } else {
            out["response_format"] = json!({"type":"json_schema","json_schema":{"name":"response","schema":format["schema"],"strict":true}});
        }
    }
    if let Some(user) = req["metadata"]["user_id"].as_str() {
        // Avoid exposing emails or other identifiers in a provider's abuse logs.
        use sha2::{Digest, Sha256};
        let user = format!("{:x}", Sha256::digest(user.as_bytes()));
        out[if responses {
            "safety_identifier"
        } else {
            "user"
        }] = json!(user);
    }
    if let Some(tier) = req["service_tier"].as_str() {
        out["service_tier"] = json!(match tier {
            "auto" => "auto",
            "standard_only" => "default",
            _ => bail!("service_tier no portable: {tier}"),
        });
    }
    reasoning(req, model, protocol, &mut out)?;
    cache_controls(req, model, &mut out);
    Ok(out)
}

fn validate_portable(req: &Value) -> Result<()> {
    if let Some(system) = req["system"].as_array() {
        ensure!(
            system
                .iter()
                .all(|b| b["type"] == "text" && b["text"].is_string()),
            "El sistema solo admite bloques de texto"
        );
    }
    for key in req
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("La petición debe ser un objeto"))?
        .keys()
    {
        ensure!(
            matches!(
                key.as_str(),
                "model"
                    | "messages"
                    | "system"
                    | "max_tokens"
                    | "stream"
                    | "tools"
                    | "tool_choice"
                    | "temperature"
                    | "top_p"
                    | "top_k"
                    | "stop_sequences"
                    | "thinking"
                    | "output_config"
                    | "output_format"
                    | "effort"
                    | "metadata"
                    | "cache_control"
                    | "service_tier"
                    | "mcp_servers"
                    | "container"
                    | "context_management"
                    | "inference_geo"
                    | "speed"
            ),
            "Campo no portable: {key}; usa Messages nativo"
        );
    }
    for field in [
        "top_k",
        "mcp_servers",
        "container",
        "context_management",
        "inference_geo",
        "speed",
    ] {
        if let Some(value) = req.get(field) {
            ensure!(
                value.is_null() || value.as_array().is_some_and(Vec::is_empty),
                "{field} no tiene equivalencia; usa Messages nativo"
            );
        }
    }
    if let Some(config) = req["output_config"].as_object() {
        for key in config.keys() {
            ensure!(
                matches!(key.as_str(), "effort" | "format"),
                "output_config.{key} no tiene equivalencia"
            );
        }
    }
    Ok(())
}

// Report unavoidable adaptations without putting synthetic instructions in the prompt.
pub fn warnings(req: &Value, model: &str, protocol: Protocol) -> Vec<&'static str> {
    if protocol == Protocol::Messages {
        return vec![];
    }
    let mut out = Vec::new();
    if req["output_config"]["effort"] == "ultra" || req["effort"] == "ultra" {
        out.push("ultra_is_not_a_provider_effort");
    }
    if req["thinking"].get("budget_tokens").is_some() {
        out.push("thinking_budget_uses_effort");
    }
    if req.get("thinking").is_some() && reasoning_model(model).is_none() && !deepseek(model) {
        out.push("thinking_control_not_supported_for_model");
    }
    if req["thinking"]["type"] == "disabled"
        && reasoning_model(model).is_some_and(|(_, none, _)| !none)
    {
        out.push("thinking_cannot_be_disabled");
    }
    if req.get("stop_sequences").is_some()
        && (protocol == Protocol::Responses || reasoning_model(model).is_some())
    {
        out.push("stop_sequences_applied_by_proxy");
    }
    if reasoning_model(model).is_some()
        && req["thinking"]["type"] != "disabled"
        && (req.get("temperature").is_some() || req.get("top_p").is_some())
    {
        out.push("sampling_uses_model_defaults");
    }
    let encoded = req.to_string();
    if encoded.contains("\"cache_control\"") {
        out.push("cache_policy_and_ttl_differ");
    }
    if encoded.contains("\"defer_loading\":true") {
        out.push("deferred_tools_loaded_eagerly");
    }
    if encoded.contains("\"type\":\"thinking\"") || encoded.contains("\"signature\"") {
        out.push("native_thinking_not_transferable");
    }
    if protocol == Protocol::Chat
        && encoded.contains("\"tool_result\"")
        && (encoded.contains("\"image\"") || encoded.contains("\"document\""))
    {
        out.push("tool_media_in_associated_user_message");
    }
    if req["metadata"].get("user_id").is_some() {
        out.push("user_id_forwarded_as_hash");
    }
    out
}

pub(crate) fn tool_names(req: &Value) -> std::collections::HashMap<String, String> {
    req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .map(|name| (content::tool_name(name), name.to_string()))
        .collect()
}

pub fn restore_tool_names(response: &mut Value, req: &Value) {
    let names = tool_names(req);
    if let Some(content) = response["content"].as_array_mut() {
        for block in content.iter_mut().filter(|b| b["type"] == "tool_use") {
            if let Some(original) = block["name"].as_str().and_then(|n| names.get(n)) {
                block["name"] = json!(original);
            }
        }
    }
}

fn cache_controls(req: &Value, model: &str, out: &mut Value) {
    // Breakpoint support is model-specific, not a property of compatible APIs.
    let explicit = reasoning_model(model).is_some()
        && ["gpt-5.6-", "gpt-6-", "gpt-6.1-"]
            .iter()
            .any(|p| model.starts_with(p));
    if !explicit {
        fn remove(value: &mut Value) {
            match value {
                Value::Object(o) => {
                    o.remove("prompt_cache_breakpoint");
                    for v in o.values_mut() {
                        remove(v);
                    }
                }
                Value::Array(a) => {
                    for v in a {
                        remove(v);
                    }
                }
                _ => {}
            }
        }
        remove(out);
        return;
    }
    // Match converted content to its source to preserve cache boundaries. Tools
    // cache at the end of the tool prefix; provider TTL has no 5m/1h equivalent.
    fn has_cache(value: &Value) -> bool {
        match value {
            Value::Object(o) => o.contains_key("cache_control") || o.values().any(has_cache),
            Value::Array(a) => a.iter().any(has_cache),
            _ => false,
        }
    }
    if !has_cache(req) {
        return;
    }
    fn has_breakpoint(value: &Value) -> bool {
        match value {
            Value::Object(o) => {
                o.contains_key("prompt_cache_breakpoint") || o.values().any(has_breakpoint)
            }
            Value::Array(a) => a.iter().any(has_breakpoint),
            _ => false,
        }
    }
    out["prompt_cache_options"] =
        json!({"mode":if has_breakpoint(out) {"explicit"} else {"implicit"},"ttl":"30m"});
}

pub fn stop(reason: &str) -> &'static str {
    match reason {
        "tool_calls" | "function_call" | "tool_use" => "tool_use",
        "length" | "max_output_tokens" => "max_tokens",
        "content_filter" | "refusal" | "safety" => "refusal",
        "stop_sequence" => "stop_sequence",
        "pause_turn" => "pause_turn",
        _ => "end_turn",
    }
}

pub fn response(value: &Value, model: &str, protocol: Protocol) -> Result<Value> {
    if protocol == Protocol::Messages {
        return Ok(value.clone());
    }
    let responses = protocol == Protocol::Responses;
    ensure!(
        value["error"].is_null(),
        "Error del proveedor: {}",
        value["error"]
    );
    let mut content = Vec::new();
    let mut reason = "end_turn";
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
                            let (block, refused) = output_part(p)?;
                            content.push(block);
                            if refused {
                                reason = "refusal";
                            }
                        }
                    }
                    if let Some(state) = responses_message_state(model, item)? {
                        content.push(state);
                    }
                }
                Some("function_call") => {
                    reason = "tool_use";
                    content.push(tool_block(
                        item["call_id"]
                            .as_str()
                            .filter(|id| !id.is_empty())
                            .or_else(|| item["id"].as_str())
                            .unwrap_or_default(),
                        item["name"].as_str().unwrap_or_default(),
                        &item["arguments"],
                    )?);
                }
                Some("reasoning") => {
                    if let Some(block) = responses_state(model, item)? {
                        content.push(block);
                    }
                }
                other => bail!(
                    "Tipo de salida Responses no portable: {}; usa Messages nativo para funciones exclusivas",
                    other.unwrap_or("sin tipo")
                ),
            }
        }
        if value["status"] == "incomplete" {
            let incomplete = stop(
                value["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or_default(),
            );
            if incomplete == "max_tokens" || reason != "tool_use" {
                reason = incomplete;
            }
        }
    } else {
        let choice = value["choices"]
            .get(0)
            .ok_or_else(|| anyhow::anyhow!("Respuesta Chat sin choices"))?;
        if deepseek(model)
            && let Some(thought) = choice["message"]["reasoning_content"].as_str()
        {
            content.push(state_block(model, "deepseek", &json!(thought))?);
        }
        {
            let mut state = serde_json::Map::new();
            for field in ["reasoning_content", "reasoning"] {
                if deepseek(model) && field == "reasoning_content" {
                    continue;
                }
                if let Some(value) = choice["message"].get(field) {
                    ensure!(
                        value.is_null() || value.is_string(),
                        "Formato {field} no portable"
                    );
                    if value.is_string() {
                        state.insert(field.into(), value.clone());
                    }
                }
            }
            if let Some(details) = choice["message"]
                .get("reasoning_details")
                .filter(|v| !v.is_null())
            {
                ensure!(
                    details
                        .as_array()
                        .is_some_and(|all| all.iter().all(Value::is_object)),
                    "reasoning_details debe ser una lista de objetos"
                );
                state.insert("reasoning_details".into(), details.clone());
            }
            if !state.is_empty() {
                content.push(state_block(model, "chat_reasoning", &json!(state))?);
            }
        }
        if let Some(parts) = choice["message"]["content"].as_array() {
            for part in parts {
                content.push(output_part(part)?.0);
            }
        } else if let Some(t) = choice["message"]["content"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            content.push(json!({"type":"text","text":t}));
        }
        let refusal = choice["message"]["refusal"]
            .as_str()
            .filter(|s| !s.is_empty());
        if let Some(t) = refusal {
            content.push(json!({"type":"text","text":t}));
        }
        let citations = annotation_text(&choice["message"]["annotations"]);
        if !citations.is_empty() {
            content.push(json!({"type":"text","text":citations}));
        }
        for field in ["audio", "function_call"] {
            ensure!(
                choice["message"][field].is_null(),
                "Salida Chat {field} no portable"
            );
        }
        if let Some(calls) = choice["message"]["tool_calls"].as_array() {
            for call in calls {
                if let Some(extra) = call.get("extra_content").filter(|v| v.is_object()) {
                    let mut metadata = serde_json::Map::new();
                    metadata.insert(
                        call["id"].as_str().unwrap_or_default().to_string(),
                        extra.clone(),
                    );
                    content.push(state_block(model, "chat_tools", &Value::Object(metadata))?);
                }
                content.push(tool_block(
                    call["id"].as_str().unwrap_or_default(),
                    call["function"]["name"].as_str().unwrap_or_default(),
                    &call["function"]["arguments"],
                )?);
            }
        }
        reason = stop(choice["finish_reason"].as_str().unwrap_or_default());
        if refusal.is_some()
            || choice["message"]["content"]
                .as_array()
                .is_some_and(|p| p.iter().any(|b| b["type"] == "refusal"))
        {
            reason = "refusal";
        }
        if reason == "end_turn" && content.iter().any(|b| b["type"] == "tool_use") {
            reason = "tool_use";
        }
    }
    if content.is_empty() {
        content.push(json!({"type":"text","text":""}));
    }
    Ok(
        json!({"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":model,"content":content,"stop_reason":reason,"stop_sequence":value.get("stop_sequence").cloned().unwrap_or(Value::Null),"usage":usage(&value["usage"],protocol)}),
    )
}

pub(crate) fn usage(raw: &Value, protocol: Protocol) -> Value {
    if protocol == Protocol::Messages {
        return raw.clone();
    }
    let chat = protocol == Protocol::Chat;
    let input = raw[if chat {
        "prompt_tokens"
    } else {
        "input_tokens"
    }]
    .as_u64()
    .unwrap_or(0);
    let output = raw[if chat {
        "completion_tokens"
    } else {
        "output_tokens"
    }]
    .as_u64()
    .unwrap_or(0);
    let input_details = &raw[if chat {
        "prompt_tokens_details"
    } else {
        "input_tokens_details"
    }];
    let cached = input_details["cached_tokens"]
        .as_u64()
        .unwrap_or(0)
        .min(input);
    // Anthropic reports uncached input separately; do not double-count cache hits.
    let mut result = json!({"input_tokens":input-cached,"output_tokens":output});
    if input_details.get("cached_tokens").is_some() {
        result["cache_read_input_tokens"] = json!(cached);
    }
    // Extensions preserve provider detail without claiming a native equivalent.
    for (from, to) in [
        (
            if chat {
                "prompt_tokens_details"
            } else {
                "input_tokens_details"
            },
            "input_tokens_details",
        ),
        (
            if chat {
                "completion_tokens_details"
            } else {
                "output_tokens_details"
            },
            "output_tokens_details",
        ),
    ] {
        if let Some(value) = raw.get(from) {
            result[to] = value.clone();
        }
    }
    result
}

pub(crate) fn output_part(part: &Value) -> Result<(Value, bool)> {
    let kind = part["type"].as_str().unwrap_or_default();
    let refused = kind == "refusal";
    ensure!(
        matches!(kind, "text" | "output_text" | "refusal"),
        "Bloque de salida no portable: {kind}"
    );
    let text = part[if refused { "refusal" } else { "text" }]
        .as_str()
        .unwrap_or_default();
    Ok((
        json!({"type":"text","text":format!("{text}{}",annotation_text(&part["annotations"]))}),
        refused,
    ))
}

pub(crate) fn annotation_text(annotations: &Value) -> String {
    let Some(all) = annotations.as_array() else {
        return String::new();
    };
    if all.is_empty() {
        return String::new();
    }
    // Keep URLs, file references, titles and offsets without forging signed
    // native citation objects. JSON also escapes untrusted line delimiters.
    format!("\n[Source annotations: {}]", json!(all))
}

pub fn apply_stops(response: &mut Value, req: &Value) -> Result<()> {
    let mut filter = StopFilter::new(req)?;
    let Some(content) = response["content"].as_array_mut() else {
        return Ok(());
    };
    let mut output = Vec::new();
    for mut block in std::mem::take(content) {
        if filter.matched.is_some() {
            break;
        }
        if block["type"] == "text" {
            let text = filter.push(block["text"].as_str().unwrap_or_default());
            block["text"] = json!(text);
            output.push(block);
        } else {
            let tail = filter.flush();
            if !tail.is_empty() {
                output.push(json!({"type":"text","text":tail}));
            }
            output.push(block);
        }
    }
    let tail = filter.flush();
    if !tail.is_empty() {
        if let Some(last) = output.last_mut().filter(|p| p["type"] == "text") {
            let text = format!("{}{tail}", last["text"].as_str().unwrap_or_default());
            last["text"] = json!(text);
        } else {
            output.push(json!({"type":"text","text":tail}));
        }
    }
    *content = output;
    if let Some(sequence) = filter.matched {
        response["stop_reason"] = json!("stop_sequence");
        response["stop_sequence"] = json!(sequence);
    }
    Ok(())
}

pub(crate) fn parse_arguments(value: &Value) -> Result<Value> {
    let s = value.as_str().ok_or_else(|| {
        anyhow::anyhow!("Los argumentos de herramienta deben ser una cadena JSON")
    })?;
    let parsed: Value = serde_json::from_str(if s.is_empty() { "{}" } else { s })?;
    ensure!(
        parsed.is_object(),
        "Los argumentos de herramienta deben ser un objeto"
    );
    Ok(parsed)
}

fn tool_block(id: &str, name: &str, arguments: &Value) -> Result<Value> {
    ensure!(
        !id.is_empty() && !name.is_empty(),
        "Herramienta sin ID o nombre"
    );
    Ok(json!({"type":"tool_use","id":id,"name":name,"input":parse_arguments(arguments)?}))
}
