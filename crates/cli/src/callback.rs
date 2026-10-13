//! Browser callbacks stay on the CLI machine; validation and tokens stay on the server.
use crate::control::Client;
use anyhow::Result;
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
struct Relay {
    client: Client,
    done: tokio::sync::Notify,
}
pub async fn listen(client: Client) -> Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let state = Arc::new(Relay {
        client,
        done: tokio::sync::Notify::new(),
    });
    let router = Router::new()
        .route("/auth/callback", get(oauth))
        .route("/", get(zed))
        .with_state(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = tokio::time::timeout(Duration::from_secs(600), state.done.notified()).await;
            })
            .await;
    });
    Ok(port)
}
async fn forward(state: Arc<Relay>, path: &str, query: HashMap<String, String>) -> Response {
    let body = serde_json::to_value(&query).unwrap();
    let mut body = body;
    if path == "/api/zed/callback" {
        let Some(id) = query.get("user_id").and_then(|v| v.parse::<u32>().ok()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        body["user_id"] = serde_json::json!(id);
    }
    match state
        .client
        .request(reqwest::Method::POST, path, Some(&body))
        .await
    {
        Ok(_) => {
            state.done.notify_one();
            "Cuenta conectada. Vuelve al CLI y actualiza los proveedores.".into_response()
        }
        // Do not echo callback parameters or provider errors into browser/logs.
        Err(_) => (
            StatusCode::BAD_REQUEST,
            "No se pudo completar la conexión. Vuelve al CLI e inténtalo de nuevo.",
        )
            .into_response(),
    }
}
async fn oauth(
    State(state): State<Arc<Relay>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    forward(state, "/api/auth/callback", query).await
}
async fn zed(
    State(state): State<Arc<Relay>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    forward(state, "/api/zed/callback", query).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, http::HeaderMap, routing::post};
    #[tokio::test]
    async fn callback_relay_uses_gateway_key_and_forwards_state_to_server() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        let router = Router::new().route(
            "/api/auth/callback",
            post(
                move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                    let send = send.clone();
                    async move {
                        send.send((headers, body)).await.unwrap();
                        Json(serde_json::json!({"connected":true}))
                    }
                },
            ),
        );
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", socket.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(socket, router).await.unwrap();
        });
        let port = listen(Client::with_key(&endpoint, "relay-test-key").unwrap())
            .await
            .unwrap();
        let response = reqwest::Client::new()
            .get(format!(
                "http://127.0.0.1:{port}/auth/callback?state=one-time-state&code=authorization-code"
            ))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let (headers, body) = receive.recv().await.unwrap();
        assert_eq!(headers["x-api-key"], "relay-test-key");
        assert_eq!(body["state"], "one-time-state");
        assert_eq!(body["code"], "authorization-code");
        let text = response.text().await.unwrap();
        assert!(!text.contains("authorization-code"));
        assert!(!text.contains("relay-test-key"));
        task.abort();
    }
}
