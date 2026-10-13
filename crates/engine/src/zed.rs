//! Zed's completion envelope and NDJSON transport, independent of networking.
use crate::{Protocol, stream::Translator, translate};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    OpenAi,
    XAi,
    Google,
}
impl Provider {
    pub fn protocol(self) -> Protocol {
        match self {
            Self::Anthropic | Self::Google => Protocol::Messages,
            Self::OpenAi | Self::XAi => Protocol::Responses,
        }
    }
}

pub fn request(req: &Value, target: &str, provider: Provider) -> Result<Value> {
    let mut body = if provider == Provider::Google {
        google::request(req, target)?
    } else {
        translate::request(req, target, provider.protocol())?
    };
    if provider == Provider::Anthropic {
        anthropic::normalize(&mut body)?;
    }
    // Zed always returns a stream of provider events, including for JSON clients.
    body["stream"] = json!(true);
    if provider == Provider::Google {
        body.as_object_mut().unwrap().remove("stream");
    }
    Ok(json!({"provider":provider,"model":target,"provider_request":body}))
}

pub fn warnings(req: &Value, target: &str, provider: Provider) -> Vec<&'static str> {
    if provider != Provider::Google {
        let mut warnings = translate::warnings(req, target, provider.protocol());
        if provider == Provider::Anthropic
            && req["messages"]
                .as_array()
                .is_some_and(|messages| messages.iter().any(|message| message["role"] == "system"))
        {
            warnings.push("system_messages_moved_to_system");
        }
        return warnings;
    }
    let mut warnings = Vec::new();
    if req.to_string().contains("\"cache_control\"") {
        warnings.push("google_cache_uses_provider_defaults");
    }
    if req["metadata"]["user_id"].is_string() {
        warnings.push("google_user_metadata_not_forwarded");
    }
    if req["stop_sequences"]
        .as_array()
        .is_some_and(|s| !s.is_empty())
    {
        warnings.push("google_finish_reason_does_not_identify_matched_stop_sequence");
    }
    warnings
}

#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    ended: bool,
    wrapped: bool,
}
impl Decoder {
    pub fn new(wrapped: bool) -> Self {
        Self {
            wrapped,
            ..Self::default()
        }
    }
    pub fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                let line = std::mem::take(&mut self.pending);
                self.line(&line, &mut events)?;
            } else {
                self.pending.push(*byte);
                ensure!(
                    self.pending.len() <= 4 * 1024 * 1024,
                    "Línea Zed demasiado grande"
                );
            }
        }
        if eof {
            let line = std::mem::take(&mut self.pending);
            self.line(&line, &mut events)?;
            ensure!(
                !self.wrapped || self.ended,
                "Zed cerró el stream sin stream_ended"
            );
        }
        Ok(events)
    }
    fn line(&mut self, bytes: &[u8], events: &mut Vec<Value>) -> Result<()> {
        let text = std::str::from_utf8(bytes)?
            .trim()
            .trim_start_matches('\u{feff}');
        if text.is_empty() {
            return Ok(());
        }
        ensure!(!self.ended, "Zed envió datos después de stream_ended");
        let value: Value = serde_json::from_str(text).context("Evento JSON de Zed inválido")?;
        if !self.wrapped {
            events.push(value);
            return Ok(());
        }
        if let Some(event) = value.get("event") {
            events.push(event.clone());
        } else if let Some(status) = value.get("status") {
            if status == "stream_ended" {
                self.ended = true;
            } else if status.get("failed").is_some() {
                let failure = &status["failed"];
                bail!(
                    "Zed: {} ({}; retry_after={})",
                    failure["message"].as_str().unwrap_or("petición fallida"),
                    failure["code"].as_str().unwrap_or("unknown"),
                    failure["retry_after"]
                );
            }
            // Queued, started and future non-terminal status messages aren't model content.
        } else {
            bail!("Envoltorio de evento Zed inválido");
        }
        Ok(())
    }
}

pub struct Stream {
    provider: Provider,
    translator: Translator,
    google: google::Stream,
    model: String,
    pending_stop: Option<Value>,
}
impl Stream {
    pub fn new(provider: Provider, model: &str, req: &Value) -> Result<Self> {
        Ok(Self {
            provider,
            translator: Translator::with_request(provider.protocol(), model, req)?,
            google: google::Stream::default(),
            model: model.into(),
            pending_stop: None,
        })
    }
    pub fn start(&self) -> Vec<Value> {
        if self.provider == Provider::Anthropic {
            vec![]
        } else {
            vec![
                json!({"type":"message_start","message":{"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":self.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}),
            ]
        }
    }
    pub fn accept(&mut self, raw: &Value) -> Result<Vec<Value>> {
        if self.provider == Provider::Google {
            return self.google.accept(raw, &self.model);
        }
        let mut events = self.translator.accept(&raw.to_string())?;
        for event in &mut events {
            if event["type"] == "message_start" {
                event["message"]["model"] = json!(self.model);
            }
        }
        events.retain(|event| {
            if event["type"] == "message_stop" {
                self.pending_stop = Some(event.clone());
                false
            } else {
                true
            }
        });
        Ok(events)
    }
    pub fn finish(&mut self) -> Result<Vec<Value>> {
        if self.provider == Provider::Google {
            self.google.finish()
        } else {
            let mut events = self.translator.finish()?;
            if let Some(stop) = self.pending_stop.take() {
                events.push(stop);
            }
            Ok(events)
        }
    }
}

// Assemble the same Anthropic events emitted to SSE clients for stream=false.
#[derive(Default)]
pub struct Collector {
    message: Value,
    blocks: std::collections::BTreeMap<u64, Value>,
    arguments: std::collections::HashMap<u64, String>,
    ended: bool,
}
impl Collector {
    pub fn accept(&mut self, event: &Value) -> Result<()> {
        let index = event["index"].as_u64().unwrap_or(0);
        match event["type"].as_str().unwrap_or("") {
            "message_start" => self.message = event["message"].clone(),
            "content_block_start" => {
                self.blocks.insert(index, event["content_block"].clone());
            }
            "content_block_delta" => {
                let block = self
                    .blocks
                    .get_mut(&index)
                    .context("Delta Zed sin bloque")?;
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "input_json_delta" => self
                        .arguments
                        .entry(index)
                        .or_default()
                        .push_str(delta["partial_json"].as_str().unwrap_or("")),
                    "text_delta" | "thinking_delta" | "signature_delta" => {
                        let key = match delta["type"].as_str().unwrap() {
                            "text_delta" => "text",
                            "thinking_delta" => "thinking",
                            _ => "signature",
                        };
                        let mut text = block[key].as_str().unwrap_or("").to_string();
                        text.push_str(delta[key].as_str().unwrap_or(""));
                        block[key] = json!(text);
                    }
                    "citations_delta" => {
                        if !block["citations"].is_array() {
                            block["citations"] = json!([]);
                        }
                        block["citations"]
                            .as_array_mut()
                            .unwrap()
                            .push(delta["citation"].clone());
                    }
                    _ => bail!("Delta Zed no soportado: {}", delta["type"]),
                }
            }
            "content_block_stop" => {
                if let Some(arguments) = self.arguments.remove(&index) {
                    self.blocks
                        .get_mut(&index)
                        .context("Herramienta sin bloque")?["input"] =
                        serde_json::from_str(&arguments)
                            .context("Argumentos de herramienta Zed incompletos")?;
                }
            }
            "message_delta" => {
                for key in ["stop_reason", "stop_sequence"] {
                    if let Some(value) = event["delta"].get(key) {
                        self.message[key] = value.clone();
                    }
                }
                if let Some(usage) = event["usage"].as_object() {
                    for (k, v) in usage {
                        self.message["usage"][k] = v.clone();
                    }
                }
            }
            "message_stop" => self.ended = true,
            "error" => bail!(
                "Zed: {}",
                event["error"]["message"]
                    .as_str()
                    .unwrap_or("error del proveedor")
            ),
            "ping" => {}
            other => bail!("Evento Anthropic desconocido: {other}"),
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Value> {
        ensure!(
            self.ended && self.message.is_object() && self.arguments.is_empty(),
            "Respuesta Zed incompleta"
        );
        self.message["content"] = json!(self.blocks.into_values().collect::<Vec<_>>());
        Ok(self.message)
    }
}

mod anthropic;
mod google;
#[cfg(test)]
mod tests;
