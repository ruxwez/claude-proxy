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

// Subscription gateways require SSE even when the Anthropic client wants JSON.
// Reuse the streaming translator so tool calls, reasoning and stops stay identical.
pub async fn collected_response(
    upstream: reqwest::Response,
    protocol: Protocol,
    model: String,
    req: &Value,
    capture: Option<crate::traces::Capture>,
) -> Result<Response> {
    let response = response_traced(upstream, protocol, model, req, capture.clone());
    let mut source = response.into_body().into_data_stream();
    let mut decoder = Decoder::default();
    let mut collector = claude_proxy_engine::zed::Collector::default();
    let mut size = 0;
    let result: Result<Response> = async {
        loop {
            let chunk = source.next().await.transpose()?;
            let eof = chunk.is_none();
            let bytes = chunk.unwrap_or_default();
            size += bytes.len();
            anyhow::ensure!(
                size <= 32 * 1024 * 1024,
                "Respuesta del proveedor demasiado grande"
            );
            for data in decoder.push(&bytes, eof)? {
                let event: Value = serde_json::from_str(&data)?;
                if event["type"] == "error" {
                    let status = match event["error"]["type"].as_str().unwrap_or("") {
                        "overloaded_error" => axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "rate_limit_error" => axum::http::StatusCode::TOO_MANY_REQUESTS,
                        "authentication_error" => axum::http::StatusCode::UNAUTHORIZED,
                        "invalid_request_error" => axum::http::StatusCode::BAD_REQUEST,
                        _ => axum::http::StatusCode::BAD_GATEWAY,
                    };
                    return Ok((status, axum::Json(event)).into_response());
                }
                collector.accept(&event)?;
            }
            if eof {
                break;
            }
        }
        let message = collector.finish()?;
        if let Some(trace) = &capture {
            trace.stage("client_response", &message);
            trace.finish("completed");
        }
        Ok(axum::Json(message).into_response())
    }
    .await;
    if result.is_err()
        && let Some(trace) = &capture
    {
        trace.finish("conversion_error");
    }
    result
}

pub async fn zed_response(
    upstream: reqwest::Response,
    provider: claude_proxy_engine::zed::Provider,
    model: String,
    req: &Value,
    capture: Option<crate::traces::Capture>,
) -> Result<Response> {
    let streaming = req["stream"].as_bool().unwrap_or(false);
    if let Some(trace) = &capture {
        trace.finish("conversion_error");
    }
    let translator = claude_proxy_engine::zed::Stream::new(provider, &model, req)?;
    if let Some(trace) = &capture {
        trace.finish("cancelled");
    }
    let wrapped = upstream
        .headers()
        .contains_key("x-zed-server-supports-status-messages");
    let failure_capture = capture.clone();
    let source = async_stream::try_stream! {
        if let Some(trace) = &capture { trace.finish("cancelled"); }
        let mut translator = translator;
        for event in translator.start() {
            if let Some(trace) = &capture { trace.stage(if streaming { "client_event" } else { "proxy_event" }, &event); }
            yield event;
        }
        let mut decoder = claude_proxy_engine::zed::Decoder::new(wrapped);
        let mut source = upstream.bytes_stream();
        let period = Duration::from_secs(15);
        let mut keepalive = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let chunk = tokio::select! {
                chunk = source.next() => chunk,
                _ = keepalive.tick() => {
                    let event = json!({"type":"ping"});
                    if streaming && let Some(trace) = &capture { trace.stage("client_event", &event); }
                    yield event; continue;
                }
            };
            let eof = chunk.is_none();
            let bytes = chunk.transpose()?.unwrap_or_default();
            for raw in decoder.push(&bytes, eof)? {
                if let Some(trace) = &capture { trace.stage("upstream_event", &raw); }
                for event in translator.accept(&raw)? {
                    if let Some(trace) = &capture { trace.stage(if streaming { "client_event" } else { "proxy_event" }, &event); }
                    if event["type"] == "error" { Err(anyhow::anyhow!("Zed: {}", event["error"]["message"].as_str().unwrap_or("error del proveedor")))?; }
                    yield event;
                }
            }
            if eof { break; }
        }
        for event in translator.finish()? {
            if let Some(trace) = &capture { trace.stage(if streaming { "client_event" } else { "proxy_event" }, &event); }
            yield event;
        }
        if let Some(trace) = &capture { trace.finish("completed"); }
    };
    if streaming {
        let body = Body::from_stream(source.map(move |result: Result<Value>| {
            Ok::<_, Infallible>(match result {
                Ok(value) => frame(&value),
                Err(error) => {
                    let event = json!({"type":"error","error":{"type":"api_error","message":format!("{error:#}")}});
                    if let Some(trace) = &failure_capture {
                        trace.finish("stream_error");
                        trace.stage("client_event", &event);
                    }
                    frame(&event)
                }
            })
        }));
        return Ok((
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-cache"),
                (header::HeaderName::from_static("x-accel-buffering"), "no"),
            ],
            body,
        )
            .into_response());
    }
    let mut collector = claude_proxy_engine::zed::Collector::default();
    let mut size = 0;
    futures_util::pin_mut!(source);
    while let Some(event) = source.next().await {
        let event: Value = match event {
            Ok(event) => event,
            Err(error) => {
                if let Some(trace) = &failure_capture {
                    trace.finish("stream_error");
                }
                return Err(error);
            }
        };
        size += serde_json::to_vec(&event)?.len();
        if size > 32 * 1024 * 1024 {
            if let Some(trace) = &failure_capture {
                trace.finish("conversion_error");
            }
            anyhow::bail!("Respuesta Zed demasiado grande");
        }
        if let Err(error) = collector.accept(&event) {
            if let Some(trace) = &failure_capture {
                trace.finish("conversion_error");
            }
            return Err(error);
        }
    }
    let result = collector.finish().inspect_err(|_| {
        if let Some(trace) = &failure_capture {
            trace.finish("conversion_error");
        }
    })?;
    if let Some(trace) = &failure_capture {
        trace.stage("client_response", &result);
    }
    Ok(axum::Json(result).into_response())
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
    let event_stage = if req["stream"].as_bool().unwrap_or(false) {
        "client_event"
    } else {
        "proxy_event"
    };
    if let Some(trace) = &capture {
        trace.finish("cancelled");
    }
    let translator = Translator::with_request(protocol, &model, req);
    let failure_capture = capture.clone();
    let stream = async_stream::try_stream! {
        let started = Instant::now();
        if let Some(trace) = &capture { trace.finish("cancelled"); }
        let native = protocol == Protocol::Messages;
        if !native {
            let event = json!({"type":"message_start","message":{"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}});
            if let Some(trace) = &capture { trace.stage(event_stage, &event); }
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
                    if let Some(trace) = &capture { trace.stage(event_stage, &event); }
                    yield frame(&event);
                    continue;
                }
            };
            let eof = chunk.is_none();
            let bytes = chunk.transpose()?.unwrap_or_default();
            for data in decoder.push(&bytes, eof)? {
                if let Some(trace) = &capture { trace.stage("upstream_event", &serde_json::from_str::<Value>(&data).unwrap_or_else(|_| json!(data))); }
                for event in translator.accept(&data)? {
                    if let Some(trace) = &capture { trace.stage(event_stage, &event); }
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
            if let Some(trace) = &capture { trace.stage(event_stage, &event); }
            yield frame(&event);
        }
        if let Some(trace) = &capture { trace.finish("completed"); }
        tracing::info!(model, elapsed_ms = started.elapsed().as_millis(), "Streaming terminado");
    };
    let body = Body::from_stream(stream.map(move |result: Result<Bytes>| {
        Ok::<_, Infallible>(match result {
            Ok(bytes) => bytes,
            Err(error) => {
                let event = json!({"type":"error","error":{"type":"api_error","message":format!("{error:#}")}});
                if let Some(trace) = &failure_capture {
                    trace.finish("stream_error");
                    trace.stage(event_stage, &event);
                }
                tracing::warn!(error = %error, cause = %error.root_cause(), "Streaming del proveedor interrumpido");
                frame(&event)
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
