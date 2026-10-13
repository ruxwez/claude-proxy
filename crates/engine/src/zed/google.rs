use super::*;
use std::collections::{HashMap, HashSet};

pub(super) fn request(req: &Value, target: &str) -> Result<Value> {
    for key in [
        "output_config",
        "context_management",
        "mcp_servers",
        "container",
        "service_tier",
    ] {
        ensure!(
            req.get(key).is_none_or(Value::is_null),
            "Zed Google no representa la opción Anthropic {key}"
        );
    }
    ensure!(
        req["tool_choice"]["disable_parallel_tool_use"] != true,
        "Zed Google no garantiza disable_parallel_tool_use"
    );
    let messages = req["messages"].as_array().context("Faltan mensajes")?;
    let mut names = HashMap::new();
    let mut native_ids = HashSet::new();
    for message in messages {
        if let Some(blocks) = message["content"].as_array() {
            for block in blocks {
                if block["type"] == "tool_use" {
                    names.insert(
                        block["id"].as_str().unwrap_or(""),
                        block["name"].as_str().unwrap_or(""),
                    );
                }
                if let Some(parts) = translate::replay_state(
                    block,
                    req["model"].as_str().unwrap_or(target),
                    "zed_google",
                )? {
                    for part in parts.as_array().unwrap() {
                        if let Some(id) = part["functionCall"]["id"].as_str() {
                            native_ids.insert(id.to_owned());
                        }
                    }
                }
            }
        }
    }
    let public_model = req["model"].as_str().unwrap_or(target);
    let mut contents = Vec::new();
    for message in messages {
        let assistant = message["role"] == "assistant";
        let blocks = message["content"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![json!({"type":"text","text":message["content"]})]);
        let mut replay = Vec::new();
        if assistant {
            for block in &blocks {
                if let Some(parts) = translate::replay_state(block, public_model, "zed_google")? {
                    replay.extend(parts.as_array().unwrap().clone());
                }
            }
        }
        let mut parts = Vec::new();
        if !replay.is_empty() {
            parts = replay;
        } else {
            for block in &blocks {
                parts.push(match block["type"].as_str().unwrap_or("") {
                    "text" => json!({"text":block["text"]}),
                    "thinking" => { let mut part = json!({"text":block["thinking"],"thought":true}); if block["signature"].is_string() { part["thoughtSignature"] = block["signature"].clone(); } part },
                    "tool_use" => json!({"functionCall":{"name":block["name"],"args":block["input"]}}),
                    "tool_result" => {
                        let id = block["tool_use_id"].as_str().context("Resultado sin tool_use_id")?;
                        let name = names.get(id).context("Resultado Google sin llamada de herramienta")?;
                        let mut part = json!({"functionResponse":{"name":name,"response":{"content":block["content"],"is_error":block["is_error"].as_bool().unwrap_or(false)}}});
                        if native_ids.contains(id) { part["functionResponse"]["id"] = json!(id); }
                        part
                    }
                    "image" | "document" if block["source"]["type"] == "base64" => json!({"inlineData":{"mimeType":block["source"]["media_type"],"data":block["source"]["data"]}}),
                    _ => bail!("Contenido no soportado por Zed Google: {}", block["type"]),
                });
            }
        }
        if !parts.is_empty() {
            contents.push(json!({"role":if assistant {"model"} else {"user"},"parts":parts}));
        }
    }
    let mut config = json!({"maxOutputTokens":req["max_tokens"]});
    for (source, dest) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("top_k", "topK"),
        ("stop_sequences", "stopSequences"),
    ] {
        if let Some(value) = req.get(source) {
            config[dest] = value.clone();
        }
    }
    if let Some(thinking) = req["thinking"]["type"].as_str() {
        config["thinkingConfig"] = match thinking {
            "enabled" => {
                ensure!(
                    req["thinking"]["budget_tokens"]
                        .as_u64()
                        .is_some_and(|n| n > 0),
                    "Indica budget_tokens positivo para Google"
                );
                json!({"includeThoughts":true,"thinkingBudget":req["thinking"]["budget_tokens"]})
            }
            "disabled" => json!({"includeThoughts":false,"thinkingBudget":0}),
            _ => bail!("Control de razonamiento Google no soportado: {thinking}"),
        };
    }
    let native_model = if target.starts_with("models/") {
        target.to_owned()
    } else {
        format!("models/{target}")
    };
    let mut out = json!({"model":native_model,"contents":contents,"generationConfig":config});
    if !req["system"].is_null() {
        out["systemInstruction"] = json!({"parts":[{"text":translate::text(&req["system"])}]});
    }
    if let Some(tools) = req["tools"].as_array() {
        let mut declarations = Vec::new();
        for tool in tools {
            ensure!(
                tool["name"].is_string() && tool["input_schema"].is_object(),
                "Google requiere herramientas con nombre y esquema"
            );
            ensure!(
                tool["strict"] != true && tool.get("type").is_none_or(|t| t == "custom"),
                "Google requiere herramientas personalizadas sin strict"
            );
            declarations.push(json!({"name":tool["name"],"description":tool["description"].as_str().unwrap_or(""),"parametersJsonSchema":tool["input_schema"]}));
        }
        out["tools"] = json!([{"functionDeclarations":declarations}]);
    }
    if let Some(choice) = req["tool_choice"]["type"].as_str() {
        let mut calling = json!({"mode":match choice { "none" => "NONE", "any" | "tool" => "ANY", "auto" => "AUTO", _ => bail!("Selección de herramienta Google inválida") }});
        if choice == "tool" {
            calling["allowedFunctionNames"] = json!([req["tool_choice"]["name"]]);
        }
        out["toolConfig"] = json!({"functionCallingConfig":calling});
    }
    Ok(out)
}

#[derive(Default)]
pub(super) struct Stream {
    index: usize,
    terminal: bool,
    tool: bool,
    reason: String,
    usage: Value,
}
impl Stream {
    fn block(&mut self, block: Value, out: &mut Vec<Value>) {
        out.push(json!({"type":"content_block_start","index":self.index,"content_block":block}));
        out.push(json!({"type":"content_block_stop","index":self.index}));
        self.index += 1;
    }
    pub(super) fn accept(&mut self, raw: &Value, model: &str) -> Result<Vec<Value>> {
        ensure!(raw["error"].is_null(), "Google rechazó la petición");
        let mut out = Vec::new();
        if raw["usageMetadata"].is_object() {
            self.usage = raw["usageMetadata"].clone();
        }
        if raw["promptFeedback"]["blockReason"].is_string() {
            self.terminal = true;
            self.reason = "refusal".into();
        }
        if let Some(candidates) = raw["candidates"].as_array() {
            for candidate in candidates
                .iter()
                .filter(|c| c["index"].as_u64().unwrap_or(0) == 0)
            {
                if let Some(parts) = candidate["content"]["parts"].as_array() {
                    for part in parts {
                        if part["functionCall"].is_object() {
                            self.tool = true;
                            let call = &part["functionCall"];
                            let id = call["id"].as_str().map(str::to_owned).unwrap_or_else(|| {
                                format!("toolu_{}", uuid::Uuid::new_v4().simple())
                            });
                            self.block(json!({"type":"tool_use","id":id,"name":call["name"],"input":call["args"]}), &mut out);
                        } else if part["text"].is_string() {
                            if part["thought"] == true {
                                self.block(json!({"type":"thinking","thinking":part["text"],"signature":part["thoughtSignature"].as_str().unwrap_or("")}), &mut out);
                            } else {
                                self.block(json!({"type":"text","text":part["text"]}), &mut out);
                            }
                        } else if part["thoughtSignature"].is_string() {
                            // Some Gemini streams deliver a signature in its own part.
                            // The replay capsule below preserves it without inventing text.
                        } else {
                            bail!("Parte de respuesta Google no soportada");
                        }
                    }
                    // Replay exact Google parts, including opaque function-call thought signatures.
                    if !parts.is_empty() {
                        self.block(
                            translate::state_block(model, "zed_google", &json!(parts))?,
                            &mut out,
                        );
                    }
                }
                if let Some(reason) = candidate["finishReason"].as_str() {
                    self.terminal = true;
                    self.reason = match reason {
                        "STOP" => {
                            if self.tool {
                                "tool_use"
                            } else {
                                "end_turn"
                            }
                        }
                        "MAX_TOKENS" => "max_tokens",
                        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
                            "refusal"
                        }
                        _ => bail!("Fin Google no soportado: {reason}"),
                    }
                    .into();
                }
            }
        }
        Ok(out)
    }
    pub(super) fn finish(&mut self) -> Result<Vec<Value>> {
        ensure!(self.terminal, "Google cerró el stream sin finishReason");
        let n = |key| self.usage[key].as_u64().unwrap_or(0);
        Ok(vec![
            json!({"type":"message_delta","delta":{"stop_reason":self.reason,"stop_sequence":null},"usage":{"input_tokens":n("promptTokenCount").saturating_sub(n("cachedContentTokenCount")),"cache_read_input_tokens":n("cachedContentTokenCount"),"output_tokens":n("candidatesTokenCount")+n("thoughtsTokenCount")}}),
            json!({"type":"message_stop"}),
        ])
    }
}
