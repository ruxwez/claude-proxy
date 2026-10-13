use crate::{
    application::Application,
    auth::Auth,
    database::Database,
    management::{self, Control},
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;

#[test]
fn database_uri_selection_never_echoes_credentials_and_requires_persistence() {
    assert!(
        matches!(Database::from_url("sqlite:///tmp/proxy.db").unwrap(),Database::Sqlite { path } if path==std::path::Path::new("/tmp/proxy.db"))
    );
    assert!(matches!(
        Database::from_url("sqlite://relative.db").unwrap(),
        Database::Sqlite { .. }
    ));
    assert!(matches!(
        Database::from_url("postgresql://u:secret@localhost/db").unwrap(),
        Database::Postgres { .. }
    ));
    assert!(matches!(
        Database::from_url("mysql://u:secret@localhost/db").unwrap(),
        Database::Mysql { .. }
    ));
    for url in [
        "sqlite::memory:",
        "sqlite:",
        "https://u:secret@host/db",
        "secret",
    ] {
        let error = Database::from_url(url).err().unwrap().to_string();
        assert!(!error.contains("secret"));
    }
}
#[tokio::test]
async fn auth_protects_both_listeners_and_unified_management_without_breaking_health() {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(
        Application::open(
            Database::Sqlite {
                path: dir.path().join("db"),
            }
            .open()
            .unwrap(),
            10800,
        )
        .unwrap(),
    );
    let control = Arc::new(Control {
        application: app,
        auth: Auth::new(Some("shared-test-key")),
        allowed_hosts: vec![],
        traces: None,
        bootstrap: None,
        configured: AtomicBool::new(true),
    });
    for router in [
        management::router_with_control(control.clone()),
        management::gateway_with_control(control.clone()),
    ] {
        for path in ["/api/status", "/api/settings", "/api/cli-config"] {
            for (header, key, expected) in [
                ("x-api-key", "wrong", StatusCode::UNAUTHORIZED),
                ("x-api-key", "shared-test-key", StatusCode::OK),
                ("authorization", "Bearer shared-test-key", StatusCode::OK),
            ] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("host", "proxy.example")
                            .header(header, key)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected, "{path}");
            }
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", "127.0.0.1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }
    let router = management::gateway_with_control(control);
    let health = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    for path in ["/v1/messages", "/v1/messages/count_tokens", "/v1/models"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"model":"none"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
#[test]
fn bearer_precedence_and_optional_key() {
    let mut headers = axum::http::HeaderMap::new();
    assert!(Auth::default().accepts(&headers));
    let auth = Auth::new(Some("key"));
    assert!(!auth.accepts(&headers));
    headers.insert("x-api-key", "key".parse().unwrap());
    assert!(auth.accepts(&headers));
    headers.insert("authorization", "Bearer wrong".parse().unwrap());
    assert!(!auth.accepts(&headers));
    headers.insert("authorization", "bEaReR key".parse().unwrap());
    assert!(auth.accepts(&headers));
}
