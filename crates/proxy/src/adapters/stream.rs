use crate::provider::Protocol;
use anyhow::Result;
use axum::{
    body::{Body, Bytes},
    http::header,
    response::{IntoResponse, Response},
};
pub use claude_proxy_engine::stream::{Decoder, Translator};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};

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

#[cfg(test)]
pub fn response(
    upstream: reqwest::Response,
    protocol: Protocol,
    model: String,
    req: &Value,
) -> Response {
    response_traced(upstream, protocol, model, req, None)
}
pub fn response_traced(
    upstream: reqwest::Response,
    protocol: Protocol,
    model: String,
    req: &Value,
    capture: Option<crate::traces::Capture>,
) -> Response {
    let translator = Translator::with_request(protocol, &model, req);
    let failure_capture = capture.clone();
    let stream = async_stream::try_stream! {
        let started = Instant::now();
        if let Some(trace) = &capture { trace.finish("cancelled"); }
        let native = protocol == Protocol::Messages;
        if !native {
            let event = json!({"type":"message_start","message":{"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}});
            if let Some(trace) = &capture { trace.stage("client_event", &event); }
            yield frame(&event);
        }
        let mut decoder = Decoder::default();
        let mut translator = translator?;
        let mut source = upstream.bytes_stream();
        let period = Duration::from_secs(15);
        let mut keepalive = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let chunk = tokio::select! {
                biased;
                chunk = source.next() => chunk,
                _ = keepalive.tick() => {
                    let event = json!({"type":"ping"});
                    if let Some(trace) = &capture { trace.stage("client_event", &event); }
                    yield frame(&event);
                    continue;
                }
            };
            let eof = chunk.is_none();
            let bytes = chunk.transpose()?.unwrap_or_default();
            for data in decoder.push(&bytes, eof)? {
                if let Some(trace) = &capture { trace.stage("upstream_event", &serde_json::from_str::<Value>(&data).unwrap_or(Value::Null)); }
                for event in translator.accept(&data)? {
                    if let Some(trace) = &capture { trace.stage("client_event", &event); }
                    if event["type"] == "error" {
                        if let Some(trace) = &capture { trace.finish("upstream_error"); }
                        yield frame(&event);
                        return;
                    }
                    yield frame(&event);
                }
            }
            if eof { break; }
        }
        for event in translator.finish()? {
            if let Some(trace) = &capture { trace.stage("client_event", &event); }
            yield frame(&event);
        }
        if let Some(trace) = &capture { trace.finish("completed"); }
        tracing::info!(model, elapsed_ms = started.elapsed().as_millis(), "Streaming terminado");
    };
    let body = Body::from_stream(stream.map(move |result: Result<Bytes>| {
        Ok::<_, Infallible>(match result {
            Ok(bytes) => bytes,
            Err(error) => {
                if let Some(trace) = &failure_capture {
                    trace.finish("stream_error");
                    trace.stage("client_event", &json!({"type":"error"}));
                }
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
