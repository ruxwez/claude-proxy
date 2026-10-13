//! Zed deserializes its own Anthropic structs before forwarding to Anthropic.
//! In particular, Message.content is a Vec, not the API's string-or-blocks union.
use super::*;

fn blocks(content: &Value) -> Result<Vec<Value>> {
    if let Some(text) = content.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    content
        .as_array()
        .cloned()
        .context("Zed Anthropic requiere contenido de texto o bloques")
}

pub(super) fn normalize(body: &mut Value) -> Result<()> {
    let mut system = Vec::new();
    let mut moved_system = false;
    if let Some(value) = body.get("system").filter(|v| !v.is_null()) {
        system = blocks(value)?;
    }
    let input = body["messages"]
        .as_array()
        .context("Faltan mensajes Anthropic")?;
    let mut messages = Vec::new();
    for original in input {
        let mut message = original.clone();
        let mut content = blocks(&message["content"])?;
        match message["role"].as_str() {
            Some("system") => {
                system.extend(content);
                moved_system = true;
                continue;
            }
            Some("user" | "assistant") => {}
            _ => bail!("Zed Anthropic solo admite roles system, user o assistant"),
        }
        for block in &mut content {
            if block["type"] == "tool_result" {
                // These fields are optional in the public API, required by Zed.
                if block.get("is_error").is_none() {
                    block["is_error"] = json!(false);
                }
                if block.get("content").is_none() {
                    block["content"] = json!("");
                }
            }
        }
        message["content"] = json!(content);
        messages.push(message);
    }
    ensure!(
        !messages.is_empty(),
        "Zed Anthropic requiere al menos un mensaje user o assistant"
    );
    if moved_system {
        ensure!(
            system
                .iter()
                .all(|block| block["type"] == "text" && block["text"].is_string()),
            "Las instrucciones system Anthropic solo admiten bloques de texto"
        );
        body["system"] = json!(system);
    }
    body["messages"] = json!(messages);
    if let Some(tools) = body["tools"].as_array_mut() {
        for tool in tools {
            if tool.get("description").is_none() {
                tool["description"] = json!("");
            }
        }
    }
    // #[serde(default)] Vec fields accept omission, but not explicit null.
    for key in ["tools", "stop_sequences"] {
        if body.get(key).is_some_and(Value::is_null) {
            body.as_object_mut().unwrap().remove(key);
        }
    }
    Ok(())
}
