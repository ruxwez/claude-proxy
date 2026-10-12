use crate::{Protocol, translate};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

// Byte-oriented framing keeps split UTF-8 characters intact. Both LF and CRLF,
// comments, multiline data, and frames split across network chunks are accepted.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    data: Option<String>,
    skip_lf: bool,
    started: bool,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for byte in bytes {
            if self.skip_lf && *byte == b'\n' {
                self.skip_lf = false;
                continue;
            }
            self.skip_lf = *byte == b'\r';
            if matches!(*byte, b'\r' | b'\n') {
                let bytes = std::mem::take(&mut self.pending);
                let line = std::str::from_utf8(&bytes)?;
                let line = if !self.started {
                    line.strip_prefix('\u{feff}').unwrap_or(line)
                } else {
                    line
                };
                self.started = true;
                self.line(line, &mut out)?;
            } else {
                self.pending.push(*byte);
                ensure!(
                    self.pending.len() <= 4 * 1024 * 1024,
                    "Evento SSE demasiado grande"
                );
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
        } else if let Some(data) = line
            .strip_prefix("data:")
            .or_else(|| (line == "data").then_some(""))
        {
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
    extra_content: Option<Value>,
    id: String,
    name: String,
    arguments: String,
    complete: bool,
    emitted: bool,
}

pub struct Translator {
    protocol: Protocol,
    model: String,
    reasoning_items: HashSet<String>,
    message_items: HashSet<String>,
    pending_reasoning: BTreeMap<usize, String>,
    deepseek_thought: Option<String>,
    chat_reasoning: serde_json::Map<String, Value>,
    thought_emitted: bool,
    next: usize,
    text_index: Option<usize>,
    tools: BTreeMap<usize, Tool>,
    items: HashMap<String, usize>,
    input_tokens: u64,
    output_tokens: u64,
    usage_details: Value,
    response_parts: BTreeMap<(usize, usize), String>,
    annotations: HashSet<String>,
    stops: translate::StopFilter,
    tool_names: HashMap<String, String>,
    reason: &'static str,
    terminal: bool,
    failed: bool,
    finished: bool,
}

impl Translator {
    pub fn new(protocol: Protocol) -> Self {
        Self::with_model(protocol, "")
    }
    pub fn with_model(protocol: Protocol, model: &str) -> Self {
        Self {
            protocol,
            model: model.into(),
            reasoning_items: HashSet::new(),
            message_items: HashSet::new(),
            pending_reasoning: BTreeMap::new(),
            deepseek_thought: None,
            chat_reasoning: serde_json::Map::new(),
            thought_emitted: false,
            next: 0,
            text_index: None,
            tools: BTreeMap::new(),
            items: HashMap::new(),
            input_tokens: 0,
            output_tokens: 0,
            usage_details: json!({}),
            response_parts: BTreeMap::new(),
            annotations: HashSet::new(),
            stops: translate::StopFilter::default(),
            tool_names: HashMap::new(),
            reason: "end_turn",
            terminal: false,
            failed: false,
            finished: false,
        }
    }
    pub fn with_request(protocol: Protocol, model: &str, req: &Value) -> Result<Self> {
        let mut translator = Self::with_model(protocol, model);
        translator.stops = translate::StopFilter::new(req)?;
        translator.tool_names = translate::tool_names(req);
        Ok(translator)
    }
    fn emit_state(&mut self, block: Value, events: &mut Vec<Value>) {
        if self.stops.matched.is_some() {
            return;
        }
        self.close_text(events);
        let index = self.next;
        self.next += 1;
        events.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        events.push(json!({"type":"content_block_stop","index":index}));
    }
    fn responses_reasoning(
        &mut self,
        key: usize,
        item: &Value,
        events: &mut Vec<Value>,
    ) -> Result<()> {
        self.pending_reasoning.remove(&key);
        if let Some(block) = translate::responses_state(&self.model, item)? {
            let key = item["id"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| item.to_string());
            if self.reasoning_items.insert(key) {
                self.emit_state(block, events);
            }
        }
        Ok(())
    }
    fn emit_thought(&mut self, events: &mut Vec<Value>) -> Result<()> {
        if !self.thought_emitted
            && let Some(thought) = &self.deepseek_thought
        {
            let block = translate::state_block(&self.model, "deepseek", &json!(thought))?;
            self.emit_state(block, events);
            self.thought_emitted = true;
        }
        Ok(())
    }
    fn emit_chat_reasoning(&mut self, events: &mut Vec<Value>) -> Result<()> {
        if !self.chat_reasoning.is_empty() {
            let state = Value::Object(std::mem::take(&mut self.chat_reasoning));
            self.emit_state(
                translate::state_block(&self.model, "chat_reasoning", &state)?,
                events,
            );
        }
        Ok(())
    }
    fn close_text(&mut self, events: &mut Vec<Value>) {
        let tail = self.stops.flush();
        self.emit_text(&tail, events);
        if let Some(index) = self.text_index.take() {
            events.push(json!({"type":"content_block_stop","index":index}));
        }
    }
    fn delta_text(&mut self, text: &str, events: &mut Vec<Value>) {
        let text = self.stops.push(text);
        self.emit_text(&text, events);
    }
    fn emit_text(&mut self, text: &str, events: &mut Vec<Value>) {
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
    fn response_text(
        &mut self,
        output: usize,
        part: usize,
        text: &str,
        full: bool,
        events: &mut Vec<Value>,
    ) -> Result<()> {
        let previous = self.response_parts.entry((output, part)).or_default();
        let delta = if full {
            ensure!(
                text.starts_with(previous.as_str()),
                "Texto final Responses inconsistente"
            );
            let delta = text[previous.len()..].to_string();
            *previous = text.into();
            delta
        } else {
            previous.push_str(text);
            text.into()
        };
        self.delta_text(&delta, events);
        Ok(())
    }
    fn annotations(&mut self, value: &Value, events: &mut Vec<Value>) {
        if let Some(all) = value.as_array() {
            for annotation in all {
                if self.annotations.insert(annotation.to_string()) {
                    self.delta_text(&translate::annotation_text(&json!([annotation])), events);
                }
            }
        }
    }
    fn response_message(
        &mut self,
        output: usize,
        item: &Value,
        events: &mut Vec<Value>,
    ) -> Result<()> {
        if let Some(parts) = item["content"].as_array() {
            for (part, value) in parts.iter().enumerate() {
                translate::output_part(value)?;
                let refused = value["type"] == "refusal";
                if refused {
                    self.reason = "refusal";
                }
                self.response_text(
                    output,
                    part,
                    value[if refused { "refusal" } else { "text" }]
                        .as_str()
                        .unwrap_or_default(),
                    true,
                    events,
                )?;
                self.annotations(&value["annotations"], events);
            }
        }
        if let Some(state) = translate::responses_message_state(&self.model, item)? {
            let key = item["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| item.to_string());
            if self.message_items.insert(key) {
                self.emit_state(state, events);
            }
        }
        Ok(())
    }
    fn tool(&mut self, key: usize, id: &str, name: &str, args: &str, full: bool) -> Result<()> {
        ensure!(
            id.is_empty()
                || !self
                    .tools
                    .iter()
                    .any(|(other, t)| *other != key && t.id == id),
            "ID de herramienta duplicado"
        );
        let tool = self.tools.entry(key).or_default();
        if tool.emitted {
            // Responses repeats a complete call in arguments.done, item.done
            // and response.completed. Only identical repetitions are allowed.
            ensure!(
                full && (id.is_empty() || id == tool.id)
                    && (name.is_empty() || name == tool.name)
                    && args == tool.arguments,
                "Herramienta modificada después de finalizar"
            );
            return Ok(());
        }
        if !id.is_empty() {
            ensure!(
                tool.id.is_empty() || tool.id == id,
                "ID de herramienta inconsistente"
            );
            tool.id = id.into();
        }
        if !name.is_empty() {
            if full || self.protocol == Protocol::Responses {
                tool.name = name.into();
            } else {
                // Chat-compatible providers can fragment the function name.
                tool.name.push_str(name);
            }
        }
        if full {
            ensure!(
                args.starts_with(&tool.arguments),
                "Argumentos finales de herramienta inconsistentes"
            );
            tool.arguments = args.into();
            tool.complete = true;
        } else if !args.is_empty() {
            ensure!(!tool.complete, "Delta de herramienta después de finalizar");
            tool.arguments.push_str(args);
        }
        ensure!(
            tool.arguments.len() <= 4 * 1024 * 1024,
            "Argumentos de herramienta demasiado grandes"
        );
        Ok(())
    }
    fn emit_tools(&mut self, events: &mut Vec<Value>) -> Result<()> {
        if self.stops.matched.is_some() {
            return Ok(());
        }
        // Parallel upstream fragments are buffered per call. Emit consecutive
        // Anthropic blocks only once complete, with validated object arguments.
        for tool in self.tools.values().filter(|t| !t.emitted) {
            if !tool.complete || tool.id.is_empty() || tool.name.is_empty() {
                break;
            }
            translate::parse_arguments(&json!(tool.arguments))?;
        }
        let ready: Vec<_> = self
            .tools
            .iter()
            .filter(|(_, t)| !t.emitted)
            .take_while(|(key, t)| {
                t.complete
                    && !t.id.is_empty()
                    && !t.name.is_empty()
                    && !self.pending_reasoning.keys().any(|index| index < *key)
            })
            .map(|(key, _)| *key)
            .collect();
        if !ready.is_empty() {
            self.close_text(events);
        }
        for key in ready {
            if let Some(extra) = self.tools[&key].extra_content.clone() {
                let mut metadata = serde_json::Map::new();
                metadata.insert(self.tools[&key].id.clone(), extra);
                self.emit_state(
                    translate::state_block(&self.model, "chat_tools", &Value::Object(metadata))?,
                    events,
                );
            }
            let tool = self.tools.get_mut(&key).unwrap();
            let name = self.tool_names.get(&tool.name).unwrap_or(&tool.name);
            let index = self.next;
            self.next += 1;
            events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":tool.id,"name":name,"input":{}}}));
            events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":if tool.arguments.is_empty() { "{}" } else { &tool.arguments }}}));
            events.push(json!({"type":"content_block_stop","index":index}));
            tool.emitted = true;
        }
        Ok(())
    }
    pub fn accept(&mut self, data: &str) -> Result<Vec<Value>> {
        ensure!(
            !self.failed && !self.finished,
            "Stream ya finalizado o fallido"
        );
        let result = self.accept_event(data);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn accept_event(&mut self, data: &str) -> Result<Vec<Value>> {
        if data == "[DONE]" {
            ensure!(
                self.protocol == Protocol::Chat,
                "Responses terminó sin evento final"
            );
            self.terminal = true;
            for tool in self.tools.values_mut() {
                tool.complete = true;
            }
            let mut events = Vec::new();
            self.emit_thought(&mut events)?;
            self.emit_chat_reasoning(&mut events)?;
            self.emit_tools(&mut events)?;
            return Ok(events);
        }
        let v: Value = serde_json::from_str(data)?;
        if self.protocol == Protocol::Messages {
            self.terminal |= v["type"] == "message_stop" || v["type"] == "error";
            return Ok(vec![v]);
        }
        if !v["error"].is_null() {
            bail!("Error del proveedor durante streaming: {}", v["error"]);
        }
        let mut events = Vec::new();
        if self.protocol == Protocol::Chat {
            if let Some(usage) = v["usage"].as_object() {
                for (key, value) in usage {
                    self.usage_details[key] = value.clone();
                }
            }
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
                        if self.reason != "refusal" {
                            self.reason = translate::stop(r);
                        }
                        self.terminal = true;
                    }
                    if !translate::deepseek(&self.model) {
                        for field in ["reasoning_content", "reasoning"] {
                            if let Some(value) = c["delta"].get(field).filter(|v| !v.is_null()) {
                                let delta = value.as_str().ok_or_else(|| {
                                    anyhow::anyhow!("Formato {field} no portable")
                                })?;
                                let entry = self
                                    .chat_reasoning
                                    .entry(field.to_string())
                                    .or_insert(json!(""));
                                let mut text = entry.as_str().unwrap_or_default().to_string();
                                text.push_str(delta);
                                ensure!(
                                    text.len() <= 2 * 1024 * 1024,
                                    "Estado de razonamiento demasiado grande"
                                );
                                *entry = json!(text);
                            }
                        }
                    }
                    if let Some(details) =
                        c["delta"].get("reasoning_details").filter(|v| !v.is_null())
                    {
                        let details = details.as_array().ok_or_else(|| {
                            anyhow::anyhow!("reasoning_details debe ser una lista")
                        })?;
                        ensure!(
                            details.iter().all(Value::is_object),
                            "reasoning_details debe contener objetos"
                        );
                        let all = self
                            .chat_reasoning
                            .entry("reasoning_details".to_string())
                            .or_insert(json!([]));
                        all.as_array_mut().unwrap().extend(details.iter().cloned());
                        ensure!(
                            serde_json::to_vec(all)?.len() <= 2 * 1024 * 1024,
                            "Estado de razonamiento demasiado grande"
                        );
                    }
                    if translate::deepseek(&self.model)
                        && let Some(thought) = c["delta"]["reasoning_content"].as_str()
                    {
                        ensure!(
                            !self.thought_emitted || thought.is_empty(),
                            "Razonamiento recibido después de finalizar"
                        );
                        let buffer = self.deepseek_thought.get_or_insert_with(String::new);
                        buffer.push_str(thought);
                        ensure!(
                            buffer.len() <= 4 * 1024 * 1024,
                            "Estado de razonamiento demasiado grande"
                        );
                    }
                    if let Some(t) = c["delta"]["content"].as_str() {
                        if !t.is_empty() {
                            self.emit_thought(&mut events)?;
                        }
                        self.delta_text(t, &mut events);
                    }
                    if let Some(parts) = c["delta"]["content"].as_array() {
                        for part in parts {
                            let (block, refused) = translate::output_part(part)?;
                            if refused {
                                self.reason = "refusal";
                            }
                            self.delta_text(
                                block["text"].as_str().unwrap_or_default(),
                                &mut events,
                            );
                        }
                    }
                    if let Some(refusal) = c["delta"]["refusal"].as_str().filter(|s| !s.is_empty())
                    {
                        self.reason = "refusal";
                        self.delta_text(refusal, &mut events);
                    }
                    self.annotations(&c["delta"]["annotations"], &mut events);
                    ensure!(
                        c["delta"]["audio"].is_null() && c["delta"]["function_call"].is_null(),
                        "Salida Chat no portable"
                    );
                    if let Some(calls) = c["delta"]["tool_calls"].as_array() {
                        // Some compatible servers announce tool metadata
                        // before the reasoning ends. Calls are buffered until
                        // completion, so emit their reasoning there as well.
                        for tc in calls {
                            let key = tc["index"].as_u64().unwrap_or(0) as usize;
                            if let Some(extra) = tc.get("extra_content").filter(|v| v.is_object()) {
                                let tool = self.tools.entry(key).or_default();
                                ensure!(
                                    !tool.emitted,
                                    "Metadatos recibidos después de finalizar la herramienta"
                                );
                                tool.extra_content = Some(extra.clone());
                            }
                            self.tool(
                                tc["index"].as_u64().unwrap_or(0) as usize,
                                tc["id"].as_str().unwrap_or_default(),
                                tc["function"]["name"].as_str().unwrap_or_default(),
                                tc["function"]["arguments"].as_str().unwrap_or_default(),
                                false,
                            )?;
                        }
                    }
                }
            }
            if self.terminal {
                for tool in self.tools.values_mut() {
                    tool.complete = true;
                }
                self.emit_thought(&mut events)?;
                self.emit_chat_reasoning(&mut events)?;
                self.emit_tools(&mut events)?;
            }
        } else {
            let output = v["output_index"].as_u64().unwrap_or(0) as usize;
            match v["type"].as_str().unwrap_or_default() {
                "response.output_text.delta" | "response.refusal.delta" => {
                    if v["type"] == "response.refusal.delta" {
                        self.reason = "refusal";
                    }
                    self.response_text(
                        output,
                        v["content_index"].as_u64().unwrap_or(0) as usize,
                        v["delta"].as_str().unwrap_or_default(),
                        false,
                        &mut events,
                    )?;
                }
                "response.output_text.done" | "response.refusal.done" => {
                    let refused = v["type"] == "response.refusal.done";
                    if refused {
                        self.reason = "refusal";
                    }
                    self.response_text(
                        output,
                        v["content_index"].as_u64().unwrap_or(0) as usize,
                        v[if refused { "refusal" } else { "text" }]
                            .as_str()
                            .unwrap_or_default(),
                        true,
                        &mut events,
                    )?;
                }
                "response.output_text.annotation.added" => {
                    self.annotations(&json!([v["annotation"]]), &mut events)
                }
                "response.content_part.done" => {
                    let part = &v["part"];
                    translate::output_part(part)?;
                    let refused = part["type"] == "refusal";
                    if refused {
                        self.reason = "refusal";
                    }
                    self.response_text(
                        output,
                        v["content_index"].as_u64().unwrap_or(0) as usize,
                        part[if refused { "refusal" } else { "text" }]
                            .as_str()
                            .unwrap_or_default(),
                        true,
                        &mut events,
                    )?;
                    self.annotations(&part["annotations"], &mut events);
                }
                "response.output_item.done" if v["item"]["type"] == "message" => {
                    self.response_message(output, &v["item"], &mut events)?;
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
                            .filter(|id| !id.is_empty())
                            .or_else(|| item["id"].as_str())
                            .unwrap_or_default(),
                        item["name"].as_str().unwrap_or_default(),
                        item["arguments"].as_str().unwrap_or_default(),
                        v["type"] == "response.output_item.done",
                    )?;
                    self.emit_tools(&mut events)?;
                }
                "response.function_call_arguments.delta"
                | "response.function_call_arguments.done" => {
                    let key = self
                        .items
                        .get(v["item_id"].as_str().unwrap_or_default())
                        .copied()
                        .unwrap_or(output);
                    self.tool(
                        key,
                        "",
                        "",
                        if v["type"] == "response.function_call_arguments.done" {
                            v["arguments"].as_str().unwrap_or_default()
                        } else {
                            v["delta"].as_str().unwrap_or_default()
                        },
                        v["type"] == "response.function_call_arguments.done",
                    )?;
                    self.emit_tools(&mut events)?;
                }
                "response.completed" | "response.incomplete" => {
                    ensure!(v["response"]["status"] != "failed", "Responses falló");
                    // Some compatible servers only provide final tool metadata
                    // in response.output. Reconcile rather than duplicate it.
                    if let Some(output) = v["response"]["output"].as_array() {
                        for (key, item) in output.iter().enumerate() {
                            if item["type"] == "message" {
                                self.response_message(key, item, &mut events)?;
                            }
                            ensure!(
                                matches!(
                                    item["type"].as_str(),
                                    Some("message" | "reasoning" | "function_call")
                                ),
                                "Salida Responses no portable: {}",
                                item["type"]
                            );
                            if item["type"] == "reasoning" {
                                self.responses_reasoning(key, item, &mut events)?;
                            }
                            if item["type"] == "function_call" {
                                let key = self
                                    .items
                                    .get(item["id"].as_str().unwrap_or_default())
                                    .copied()
                                    .unwrap_or(key);
                                self.tool(
                                    key,
                                    item["call_id"]
                                        .as_str()
                                        .filter(|id| !id.is_empty())
                                        .or_else(|| item["id"].as_str())
                                        .unwrap_or_default(),
                                    item["name"].as_str().unwrap_or_default(),
                                    item["arguments"].as_str().unwrap_or_default(),
                                    true,
                                )?;
                                self.emit_tools(&mut events)?;
                            }
                        }
                    }
                    self.input_tokens =
                        v["response"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
                    self.output_tokens = v["response"]["usage"]["output_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    if v["response"]["usage"].is_object() {
                        self.usage_details = v["response"]["usage"].clone();
                    }
                    if v["type"] == "response.incomplete" || v["response"]["status"] == "incomplete"
                    {
                        self.reason = translate::stop(
                            v["response"]["incomplete_details"]["reason"]
                                .as_str()
                                .unwrap_or_default(),
                        );
                    }
                    self.terminal = true;
                    self.emit_tools(&mut events)?;
                }
                "response.failed" | "error" => bail!("Responses falló: {}", v["response"]["error"]),
                // Only completed items contain replayable encrypted state.
                "response.output_item.done" if v["item"]["type"] == "reasoning" => {
                    self.responses_reasoning(output, &v["item"], &mut events)?;
                    self.emit_tools(&mut events)?;
                }
                "response.output_item.added" if v["item"]["type"] == "reasoning" => {
                    self.pending_reasoning
                        .insert(output, v["item"]["id"].as_str().unwrap_or_default().into());
                }
                kind if kind.starts_with("response.reasoning") => {}
                "response.output_item.added" => {
                    ensure!(
                        v["item"]["type"] == "message",
                        "Salida Responses no portable: {}",
                        v["item"]["type"]
                    );
                    self.close_text(&mut events);
                }
                "response.output_item.done" => {
                    bail!("Salida Responses no portable: {}", v["item"]["type"])
                }
                "response.content_part.added" => {
                    translate::output_part(&v["part"])?;
                }
                kind if kind.starts_with("response.audio")
                    || kind.starts_with("response.image") =>
                {
                    bail!("Evento Responses no portable: {kind}")
                }
                _ => {}
            }
        }
        Ok(events)
    }
    pub fn finish(&mut self) -> Result<Vec<Value>> {
        ensure!(
            !self.failed && !self.finished,
            "Stream ya finalizado o fallido"
        );
        ensure!(
            self.terminal,
            "El proveedor cerró el stream sin terminar el mensaje"
        );
        ensure!(
            self.stops.matched.is_some() || self.tools.values().all(|t| t.emitted),
            "Herramienta incompleta en stream"
        );
        if self.protocol == Protocol::Messages {
            self.finished = true;
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        self.close_text(&mut events);
        if self.stops.matched.is_some() {
            self.reason = "stop_sequence";
        }
        if !self.tools.is_empty()
            && !matches!(self.reason, "max_tokens" | "refusal" | "stop_sequence")
        {
            self.reason = "tool_use";
        }
        self.usage_details[if self.protocol == Protocol::Chat {
            "prompt_tokens"
        } else {
            "input_tokens"
        }] = json!(self.input_tokens);
        self.usage_details[if self.protocol == Protocol::Chat {
            "completion_tokens"
        } else {
            "output_tokens"
        }] = json!(self.output_tokens);
        events.push(json!({"type":"message_delta","delta":{"stop_reason":self.reason,"stop_sequence":self.stops.matched},"usage":translate::usage(&self.usage_details,self.protocol)}));
        events.push(json!({"type":"message_stop"}));
        self.finished = true;
        Ok(events)
    }
}
