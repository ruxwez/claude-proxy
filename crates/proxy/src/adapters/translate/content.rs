use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{blocks, image, text};

pub(super) fn text_part(text: impl Into<String>, responses: bool) -> Value {
    json!({"type":if responses {"input_text"} else {"text"},"text":text.into()})
}

// Keep source information readable without inventing Anthropic citation signatures.
pub(super) fn cited_text(block: &Value) -> String {
    let mut result = block["text"].as_str().unwrap_or_default().to_string();
    if let Some(citations) = block["citations"].as_array().filter(|v| !v.is_empty()) {
        result.push_str("\n[Source citations: ");
        result.push_str(&json!(citations).to_string());
        result.push(']');
    }
    result
}

pub(super) fn input_parts(value: &Value, responses: bool, tools: &Value) -> Result<Vec<Value>> {
    let mut parts = Vec::new();
    for block in blocks(value).iter() {
        match block["type"].as_str().unwrap_or_default() {
            "text" => parts.push(text_part(cited_text(block), responses)),
            "image" => {
                let url = image(block)?;
                parts.push(if responses {
                    json!({"type":"input_image","image_url":url})
                } else {
                    json!({"type":"image_url","image_url":{"url":url}})
                });
            }
            "document" => document(block, responses, tools, &mut parts)?,
            "search_result" => {
                parts.push(text_part(
                    format!(
                        "[Search result: {}]\nSource: {}",
                        text(&block["title"]),
                        text(&block["source"])
                    ),
                    responses,
                ));
                parts.extend(input_parts(&block["content"], responses, tools)?);
            }
            "tool_reference" => {
                let name = block["tool_name"].as_str().unwrap_or_default();
                let definition = tools
                    .as_array()
                    .and_then(|all| all.iter().find(|t| t["name"] == name));
                ensure!(
                    definition.is_some(),
                    "tool_reference sin definición: {name}"
                );
                // Deferred tools are loaded eagerly on these protocols. Keep the
                // discovery result rather than silently dropping the reference.
                parts.push(text_part(format!("[Available tool: {name}]"), responses));
            }
            other => bail!("Bloque no portable: {other}; usa Messages nativo para esta función"),
        }
        if block.get("cache_control").is_some()
            && let Some(last) = parts.last_mut()
        {
            last["prompt_cache_breakpoint"] = json!({"mode":"explicit"});
        }
    }
    Ok(parts)
}

fn document(block: &Value, responses: bool, tools: &Value, parts: &mut Vec<Value>) -> Result<()> {
    let source = &block["source"];
    for field in ["title", "context"] {
        if let Some(value) = block[field].as_str().filter(|v| !v.is_empty()) {
            parts.push(text_part(format!("[Document {field}: {value}]"), responses));
        }
    }
    match source["type"].as_str() {
        Some("text") => {
            let data = source["data"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Documento de texto sin data"))?;
            parts.push(text_part(data, responses));
        }
        Some("content") => parts.extend(input_parts(&source["content"], responses, tools)?),
        Some("base64") => {
            let media = source["media_type"].as_str().unwrap_or_default();
            let data = source["data"].as_str().unwrap_or_default();
            ensure!(
                !media.is_empty() && !data.is_empty(),
                "Documento base64 sin media_type o data"
            );
            let filename = block["title"]
                .as_str()
                .filter(|s| s.ends_with(".pdf"))
                .unwrap_or("document.pdf");
            ensure!(
                media == "application/pdf",
                "Documento {media} no portable; usa PDF o texto"
            );
            let data = format!("data:{media};base64,{data}");
            parts.push(if responses {
                json!({"type":"input_file","filename":filename,"file_data":data})
            } else {
                json!({"type":"file","file":{"filename":filename,"file_data":data}})
            });
        }
        Some("url") => {
            ensure!(
                responses,
                "Los documentos por URL requieren Responses o Messages nativo"
            );
            let url = source["url"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Documento sin URL"))?;
            parts.push(json!({"type":"input_file","file_url":url}));
        }
        Some("file") => bail!(
            "Los file_id de Anthropic no son transferibles; envía el archivo como base64 o URL"
        ),
        _ => bail!("Origen de documento no soportado"),
    }
    Ok(())
}

pub(super) fn tool_result(
    block: &Value,
    responses: bool,
    tools: &Value,
) -> Result<(Value, Vec<Value>)> {
    let id = block["tool_use_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("tool_result sin tool_use_id"))?;
    let mut parts = input_parts(&block["content"], responses, tools)?;
    if block["is_error"] == true {
        parts.insert(
            0,
            text_part("[Tool execution failed; is_error=true]", responses),
        );
    }
    if block.get("cache_control").is_some()
        && let Some(last) = parts.last_mut()
    {
        last["prompt_cache_breakpoint"] = json!({"mode":"explicit"});
    }
    if responses {
        let output = if parts
            .iter()
            .all(|p| p["type"] == "input_text" && p.get("prompt_cache_breakpoint").is_none())
        {
            json!(
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        } else {
            json!(parts)
        };
        return Ok((
            json!({"type":"function_call_output","call_id":id,"output":output}),
            vec![],
        ));
    }
    // Chat tool messages only accept text. Send media in an associated user
    // message AFTER every parallel result, keeping tool adjacency valid.
    let has_media = parts.iter().any(|p| p["type"] != "text");
    let output = parts
        .iter()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let attachment = if has_media {
        parts.insert(
            0,
            text_part(format!("[Content returned by tool {id}]"), false),
        );
        parts
    } else {
        vec![]
    };
    Ok((
        json!({"role":"tool","tool_call_id":id,"content":output}),
        attachment,
    ))
}

pub(super) fn tools(value: &Value, responses: bool) -> Result<Value> {
    let mut out = Vec::new();
    let mut names = std::collections::HashSet::new();
    for tool in value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tools debe ser una lista"))?
    {
        let kind = tool["type"].as_str().unwrap_or("custom");
        ensure!(
            kind == "custom",
            "Herramienta {kind} no portable; usa Messages nativo"
        );
        let name = tool["name"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Herramienta sin nombre"))?;
        let encoded = tool_name(name);
        ensure!(
            names.insert(encoded.clone()),
            "Nombre de herramienta duplicado o ambiguo: {name}"
        );
        if let Some(callers) = tool["allowed_callers"].as_array() {
            ensure!(
                callers.iter().any(|c| c == "direct"),
                "{name} requiere ejecución programática del servidor; usa Messages nativo"
            );
        }
        let mut description = tool["description"].as_str().unwrap_or_default().to_string();
        if encoded != name {
            description.push_str(&format!("\nOriginal tool name: {name}"));
        }
        if let Some(examples) = tool.get("input_examples") {
            description.push_str("\nInput examples: ");
            description.push_str(&examples.to_string());
        }
        let mut function = json!({"name":encoded,"description":description,"parameters":tool.get("input_schema").cloned().unwrap_or(json!({"type":"object","properties":{}}))});
        // Responses defaults omitted strict to true; Anthropic defaults false.
        if responses || tool.get("strict").is_some() {
            function["strict"] = json!(tool["strict"].as_bool().unwrap_or(false));
        }
        out.push(if responses {
            function["type"] = json!("function");
            function
        } else {
            json!({"type":"function","function":function})
        });
    }
    Ok(json!(out))
}

pub(super) fn tool_name(name: &str) -> String {
    if name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return name.into();
    }
    let prefix: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(40)
        .collect();
    let hash = format!("{:x}", Sha256::digest(name.as_bytes()));
    format!("{prefix}_{}", &hash[..20])
}
