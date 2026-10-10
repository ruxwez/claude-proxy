use crate::{provider::Protocol, translate};
use anyhow::{Result, bail, ensure};
use axum::{
    body::{Body, Bytes},
    http::header,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    time::{Duration, Instant},
};

// Byte-oriented framing keeps split UTF-8 characters intact. Both LF and CRLF,
// comments, multiline data, and frames split across network chunks are accepted.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    data: Option<String>,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        ensure!(
            self.pending.len() <= 4 * 1024 * 1024,
            "Evento SSE demasiado grande"
        );
        let mut out = Vec::new();
        if let Some(end) = self.pending.iter().rposition(|c| *c == b'\n') {
            let complete: Vec<_> = self.pending.drain(..=end).collect();
            for line in complete.split_inclusive(|c| *c == b'\n') {
                self.line(
                    std::str::from_utf8(line)?.trim_end_matches(['\r', '\n']),
                    &mut out,
                )?;
            }
        }
        if eof {
            let bytes = std::mem::take(&mut self.pending);
            if !bytes.is_empty() {
                self.line(
                    std::str::from_utf8(&bytes)?.trim_end_matches('\r'),
                    &mut out,
                )?;
            }
            self.line("", &mut out)?;
        }
        Ok(out)
    }
    fn line(&mut self, line: &str, out: &mut Vec<String>) -> Result<()> {
        if line.is_empty() {
            if let Some(data) = self.data.take() {
                out.push(data);
            }
        } else if let Some(data) = line.strip_prefix("data:") {
            let buffer = match self.data.as_mut() {
                Some(buffer) => {
                    buffer.push('\n');
                    buffer
                }
                None => self.data.insert(String::new()),
            };
            buffer.push_str(data.strip_prefix(' ').unwrap_or(data));
            ensure!(
                buffer.len() <= 4 * 1024 * 1024,
                "Evento SSE demasiado grande"
            );
        }
        Ok(())
    }
}

#[derive(Default)]
struct Tool {
    index: Option<usize>,
    id: String,
    name: String,
    pending: String,
    sent_arguments: bool,
}

pub struct Translator {
    protocol: Protocol,
    next: usize,
    text_index: Option<usize>,
    tools: HashMap<usize, Tool>,
    items: HashMap<String, usize>,
    input_tokens: u64,
    output_tokens: u64,
    reason: &'static str,
    terminal: bool,
}

impl Translator {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            next: 0,
            text_index: None,
            tools: HashMap::new(),
            items: HashMap::new(),
            input_tokens: 0,
            output_tokens: 0,
            reason: "end_turn",
            terminal: false,
        }
    }
    fn close_text(&mut self, events: &mut Vec<Value>) {
        if let Some(index) = self.text_index.take() {
            events.push(json!({"type":"content_block_stop","index":index}));
        }
    }
    fn delta_text(&mut self, text: &str, events: &mut Vec<Value>) {
        if text.is_empty() {
            return;
        }
        let index = if let Some(index) = self.text_index {
            index
        } else {
            let index = self.next;
            self.next += 1;
            self.text_index = Some(index);
            events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}));
            index
        };
        events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}));
    }
    fn tool(
        &mut self,
        key: usize,
        id: &str,
        name: &str,
        args: &str,
        full: bool,
        events: &mut Vec<Value>,
    ) {
        self.close_text(events);
        let tool = self.tools.entry(key).or_default();
        if !id.is_empty() {
            tool.id = id.into();
        }
        if !name.is_empty() && tool.index.is_none() {
            tool.name.push_str(name);
        }
        // .done repeats full arguments after streamed deltas: never append twice.
        if !full || !tool.sent_arguments {
            tool.pending.push_str(args);
        }
        if tool.index.is_none() && !tool.name.is_empty() && !tool.id.is_empty() {
            let index = self.next;
            self.next += 1;
            tool.index = Some(index);
            events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":tool.id,"name":tool.name,"input":{}}}));
        }
        if let Some(index) = tool.index
            && !tool.pending.is_empty()
        {
            events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":std::mem::take(&mut tool.pending)}}));
            tool.sent_arguments = true;
        }
    }
    pub fn accept(&mut self, data: &str) -> Result<Vec<Value>> {
        if data == "[DONE]" {
            self.terminal = true;
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data)?;
        if !v["error"].is_null() {
            bail!("Error del proveedor durante streaming: {}", v["error"]);
        }
        let mut events = Vec::new();
        if self.protocol == Protocol::Chat {
            if let Some(n) = v["usage"]["prompt_tokens"].as_u64() {
                self.input_tokens = n;
            }
            if let Some(n) = v["usage"]["completion_tokens"].as_u64() {
                self.output_tokens = n;
            }
            if let Some(choices) = v["choices"].as_array() {
                for c in choices {
                    if c["index"].as_u64().unwrap_or(0) != 0 {
                        continue;
                    }
                    if let Some(r) = c["finish_reason"].as_str() {
                        self.reason = translate::stop(r);
                        self.terminal = true;
                    }
                    if let Some(t) = c["delta"]["content"].as_str() {
                        self.delta_text(t, &mut events);
                    }
                    if let Some(calls) = c["delta"]["tool_calls"].as_array() {
                        for tc in calls {
                            self.tool(
                                tc["index"].as_u64().unwrap_or(0) as usize,
                                tc["id"].as_str().unwrap_or_default(),
                                tc["function"]["name"].as_str().unwrap_or_default(),
                                tc["function"]["arguments"].as_str().unwrap_or_default(),
                                false,
                                &mut events,
                            );
                        }
                    }
                }
            }
        } else {
            let output = v["output_index"].as_u64().unwrap_or(0) as usize;
            match v["type"].as_str().unwrap_or_default() {
                "response.output_text.delta" => {
                    self.delta_text(v["delta"].as_str().unwrap_or_default(), &mut events)
                }
                "response.output_item.added" | "response.output_item.done"
                    if v["item"]["type"] == "function_call" =>
                {
                    let item = &v["item"];
                    if let Some(id) = item["id"].as_str() {
                        self.items.insert(id.into(), output);
                    }
                    self.tool(
                        output,
                        item["call_id"]
                            .as_str()
                            .or_else(|| item["id"].as_str())
                            .unwrap_or_default(),
                        item["name"].as_str().unwrap_or_default(),
                        item["arguments"].as_str().unwrap_or_default(),
                        v["type"] == "response.output_item.done",
                        &mut events,
                    );
                }
                "response.function_call_arguments.delta" => {
                    let key = self
                        .items
                        .get(v["item_id"].as_str().unwrap_or_default())
                        .copied()
                        .unwrap_or(output);
                    self.tool(
                        key,
                        "",
                        "",
                        v["delta"].as_str().unwrap_or_default(),
                        false,
                        &mut events,
                    );
                }
                "response.completed" | "response.incomplete" => {
                    self.input_tokens =
                        v["response"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
                    self.output_tokens = v["response"]["usage"]["output_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    if v["type"] == "response.incomplete" {
                        self.reason = translate::stop(
                            v["response"]["incomplete_details"]["reason"]
                                .as_str()
                                .unwrap_or_default(),
                        );
                    }
                    self.terminal = true;
                }
                "response.failed" | "error" => bail!("Responses falló: {}", v["response"]["error"]),
                _ => {}
            }
        }
        Ok(events)
    }
    pub fn finish(&mut self) -> Result<Vec<Value>> {
        ensure!(
            self.terminal,
            "El proveedor cerró el stream sin terminar el mensaje"
        );
        ensure!(
            self.tools.values().all(|t| t.index.is_some()),
            "Herramienta incompleta en stream"
        );
        let mut events = Vec::new();
        self.close_text(&mut events);
        let mut indexes: Vec<_> = self.tools.values().filter_map(|t| t.index).collect();
        indexes.sort_unstable();
        for index in indexes {
            events.push(json!({"type":"content_block_stop","index":index}));
        }
        if !self.tools.is_empty() {
            self.reason = "tool_use";
        }
        events.push(json!({"type":"message_delta","delta":{"stop_reason":self.reason,"stop_sequence":null},"usage":{"input_tokens":self.input_tokens,"output_tokens":self.output_tokens}}));
        events.push(json!({"type":"message_stop"}));
        Ok(events)
    }
}

fn frame(value: &Value) -> Bytes {
    Bytes::from(format!(
        "event: {}\ndata: {}\n\n",
        value["type"].as_str().unwrap_or("error"),
        value
    ))
}
fn error_frame(message: &str) -> Bytes {
    frame(&json!({"type":"error","error":{"type":"api_error","message":message}}))
}

pub fn response(upstream: reqwest::Response, protocol: Protocol, model: String) -> Response {
    let stream = async_stream::try_stream! {
        let started = Instant::now();
        let native = protocol == Protocol::Messages;
        if !native {
            yield frame(&json!({"type":"message_start","message":{"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}));
        }
        let mut decoder = Decoder::default();
        let mut translator = Translator::new(protocol);
        let mut native_done = false;
        let mut source = upstream.bytes_stream();
        let period = Duration::from_secs(15);
        let mut keepalive = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let chunk = tokio::select! {
                biased;
                chunk = source.next() => chunk,
                _ = keepalive.tick() => {
                    yield frame(&json!({"type":"ping"}));
                    continue;
                }
            };
            let eof = chunk.is_none();
            let bytes = chunk.transpose()?.unwrap_or_default();
            for data in decoder.push(&bytes, eof)? {
                if native {
                    let event: Value = serde_json::from_str(&data)?;
                    if event["type"] == "error" {
                        Err(anyhow::anyhow!("Error del proveedor durante streaming: {}", event["error"]))?;
                    }
                    native_done |= event["type"] == "message_stop";
                    yield frame(&event);
                } else {
                    for event in translator.accept(&data)? { yield frame(&event); }
                }
            }
            if eof { break; }
        }
        if native {
            if !native_done {
                Err(anyhow::anyhow!("El proveedor cerró el stream sin terminar el mensaje"))?;
            }
        } else {
            for event in translator.finish()? { yield frame(&event); }
        }
        tracing::info!(model, elapsed_ms = started.elapsed().as_millis(), "Streaming terminado");
    };
    let body = Body::from_stream(stream.map(|result: Result<Bytes>| {
        Ok::<_, Infallible>(match result {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(error = %error, cause = %error.root_cause(), "Streaming del proveedor interrumpido");
                error_frame(&format!("{error:#}"))
            }
        })
    }));
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        body,
    )
        .into_response()
}
