use crate::{Args, Model, configured, fetch_models, filter_models, run, urls};
use axum::{Json, Router, extract::Query, routing::get};
use serde_json::{Value, json};
use std::{collections::HashMap, fs};

async fn mock(value: Value) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/models.json",
            get(|| async { Json(json!({"providers":{"p":{"models":{"a":{"limit":{"context":1_000_000}}}}},"models":{}})) }),
        )
        .route(
            "/v1/models",
            get(move || {
                let v = value.clone();
                async { Json(v) }
            }),
        );
    serve(app).await
}
async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, task)
}
fn models() -> Vec<Model> {
    vec![
        Model {
            id: "opencode-go/deepseek-v4.1-flash".into(),
            display_name: "DeepSeek".into(),
        },
        Model {
            id: "opencode-go/claude-haiku-5-5".into(),
            display_name: "Haiku".into(),
        },
    ]
}
fn args(base: &str, path: std::path::PathBuf) -> Args {
    Args {
        models_url: format!("{base}/models.json"),
        endpoint: base.into(),
        settings: Some(path),
        default_model: None,
        dry_run: false,
        timeout: 2,
        api_key: "local".into(),
    }
}

#[test]
fn base_url_normalization_and_validation() {
    for s in [
        "http://localhost:10800",
        "http://localhost:10800/",
        "http://localhost:10800/v1",
        "http://localhost:10800/v1/models?limit=1",
    ] {
        let (base, url) = urls(s).unwrap();
        assert_eq!(base, "http://localhost:10800");
        assert_eq!(url.as_str(), "http://localhost:10800/v1/models");
    }
    assert!(urls("file:///tmp/settings").is_err());
    assert!(urls("http://user:pass@localhost").is_err());
}

#[test]
fn preserves_preferences_and_updates_retired_aliases() {
    let old = json!({"theme":"dark","permissions":{"deny":["Bash(rm *)"]},"model":"retired","env":{"CUSTOM":"keep","ANTHROPIC_DEFAULT_OPUS_MODEL":"retired","ANTHROPIC_DEFAULT_OPUS_MODEL_NAME":"stale"}});
    let next = configured(
        old.clone(),
        &models(),
        "http://localhost:10800",
        "local",
        None,
    )
    .unwrap();
    assert_eq!(next["theme"], old["theme"]);
    assert_eq!(next["permissions"], old["permissions"]);
    assert_eq!(next["env"]["CUSTOM"], "keep");
    assert_eq!(next["model"], "opencode-go/deepseek-v4.1-flash");
    assert_eq!(
        next["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"],
        "opencode-go/claude-haiku-5-5"
    );
    assert!(
        next["env"]
            .get("ANTHROPIC_DEFAULT_OPUS_MODEL_NAME")
            .is_none()
    );
    assert_eq!(next["modelPicker"]["options"].as_array().unwrap().len(), 2);
    assert_eq!(next["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "990000");
    assert_eq!(next["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"], "900000");
    assert_eq!(
        configured(
            next.clone(),
            &models(),
            "http://localhost:10800",
            "local",
            None
        )
        .unwrap(),
        next
    );
    assert!(
        configured(
            old,
            &models(),
            "http://localhost:10800",
            "local",
            Some("missing")
        )
        .is_err()
    );
    assert!(
        configured(
            json!({"env":null}),
            &models(),
            "http://localhost:10800",
            "local",
            None
        )
        .is_err()
    );
}

#[test]
fn context_filter_uses_exact_provider_limits_and_explicit_canonical_links() {
    let contexts = json!({"providers":{
        "p":{"models":{
            "exact":{"limit":{"context":1_000_000}},
            "large":{"limit":{"context":1_048_576}},
            "small":{"limit":{"context":999_999},"canonical_model_id":"vendor/large"},
            "muse-contributor":{"limit":{"context":1_048_576}},
            "linked":{"canonical_model_id":"vendor/large"},
            "luna":{"limit":{"context":1_050_000,"input":922_000}},
            "boundary":{"limit":{"context":1_000_000,"input":990_000}},
            "invalid":{"limit":{"context":"1000000"},"canonical_model_id":"vendor/large"},
            "invalid-input":{"limit":{"context":1_000_000,"input":"unknown"}},
            "linked-input":{"limit":{"context":1_000_000},"canonical_model_id":"vendor/luna"}
        }},
        "other":{"models":{"unknown":{"limit":{"context":1_000_000}}}}
    },"models":{
        "vendor/large":{"limit":{"context":1_048_576}},
        "vendor/luna":{"limit":{"context":1_050_000,"input":922_000}}
    }});
    let models = [
        "p/exact",
        "p/large",
        "p/small",
        "p/unknown",
        "p/muse-contributor",
        "p/linked",
        "p/luna",
        "p/boundary",
        "p/invalid",
        "p/invalid-input",
        "p/linked-input",
        "p/base-contributor",
    ]
    .into_iter()
    .map(|id| Model {
        id: id.into(),
        display_name: String::new(),
    })
    .collect();
    let (accepted, excluded) = filter_models(models, &contexts);
    assert_eq!(
        accepted.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        [
            "p/exact",
            "p/large",
            "p/muse-contributor",
            "p/linked",
            "p/boundary"
        ]
    );
    assert_eq!(excluded.len(), 7);
    assert!(
        excluded
            .iter()
            .any(|s| s.contains("unknown: contexto desconocido"))
    );
}

#[tokio::test]
async fn failed_or_unmatched_metadata_preserves_settings() {
    for metadata in [
        json!({}),
        json!({"providers":{"p":{"models":{"a":{"limit":{"context":200_000}}}}},"models":{}}),
        json!({"providers":{"p":{"models":{"other":{"limit":{"context":1_000_000}}}}},"models":{}}),
        json!({"providers":{"p":{"models":{"a":{"limit":{"context":"1000000"}}}}},"models":{}}),
    ] {
        let app = Router::new()
            .route(
                "/v1/models",
                get(|| async { Json(json!({"data":[{"id":"p/a"}]})) }),
            )
            .route(
                "/models.json",
                get(move || {
                    let metadata = metadata.clone();
                    async { Json(metadata) }
                }),
            );
        let (base, task) = serve(app).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{\"theme\":\"keep\"}").unwrap();
        let mut options = args(&base, path.clone());
        assert!(run(&options).await.is_err());
        options.models_url = format!("{base}/missing");
        assert!(run(&options).await.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{\"theme\":\"keep\"}");
        assert!(!dir.path().join("backups").exists());
        task.abort();
    }
}

#[test]
fn filtered_aliases_are_replaced_and_compaction_stays_enabled() {
    let next = configured(
        json!({"model":"p/retired","env":{
            "ANTHROPIC_DEFAULT_HAIKU_MODEL":"p/retired",
            "CLAUDE_CODE_SUBAGENT_MODEL":"p/retired",
            "DISABLE_COMPACT":"1", "DISABLE_AUTO_COMPACT":"1",
            "CLAUDE_CODE_DISABLE_1M_CONTEXT":"1"
        }}),
        &models(),
        "http://localhost:10800",
        "local",
        None,
    )
    .unwrap();
    assert_eq!(next["env"]["CLAUDE_CODE_SUBAGENT_MODEL"], next["model"]);
    for key in [
        "DISABLE_COMPACT",
        "DISABLE_AUTO_COMPACT",
        "CLAUDE_CODE_DISABLE_1M_CONTEXT",
    ] {
        assert!(next["env"].get(key).is_none());
    }
}

#[tokio::test]
async fn fetches_all_pages_deduplicated_and_sorted() {
    let app = Router::new().route(
        "/v1/models",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            assert_eq!(q.get("refresh").unwrap(), "true");
            Json(if q.contains_key("after_id") {
                json!({"data":[{"id":"p/b"},{"id":"p/a"}],"has_more":false})
            } else {
                json!({"data":[{"id":"p/a"}],"has_more":true,"last_id":"p/a"})
            })
        }),
    );
    let (base, task) = serve(app).await;
    let found = fetch_models(urls(&base).unwrap().1, 2, "local")
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["p/a", "p/b"]
    );
    task.abort();
}

#[tokio::test]
async fn atomic_update_backups_idempotence_and_dry_run() {
    let (base, task) =
        mock(json!({"data":[{"id":"p/a","display_name":"A"}],"has_more":false})).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut opts = args(&base, path.clone());
    let original = b"{\"theme\":\"dark\",\"permissions\":{\"deny\":[\"Bash(rm *)\"]}}";
    fs::write(&path, original).unwrap();
    opts.dry_run = true;
    let preview = run(&opts).await.unwrap();
    assert!(preview.changed);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!dir.path().join("backups").exists());
    opts.dry_run = false;
    let report = run(&opts).await.unwrap();
    assert_eq!(fs::read(report.backup.unwrap()).unwrap(), original);
    let next: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(next["theme"], "dark");
    assert_eq!(next["model"], "p/a");
    let unchanged = run(&opts).await.unwrap();
    assert!(!unchanged.changed);
    assert!(unchanged.backup.is_none());
    task.abort();
}

#[tokio::test]
async fn refuses_empty_partial_invalid_and_looping_catalogs_without_touching_settings() {
    for value in [
        json!({"data":[]}),
        json!({"data":[{"id":"p/a"}],"errors":[{"connector":"failed"}]}),
        json!({"data":[{"id":" "}]}),
        json!({"data":[{"id":"p/a"}],"has_more":true,"last_id":"p/a"}),
    ] {
        let (base, task) = mock(value).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = b"{\"model\":\"keep\"}";
        fs::write(&path, original).unwrap();
        assert!(run(&args(&base, path.clone())).await.is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!dir.path().join("backups").exists());
        task.abort();
    }
}

#[tokio::test]
async fn invalid_settings_are_never_overwritten() {
    let (base, task) = mock(json!({"data":[{"id":"p/a"}]})).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, b"broken json").unwrap();
    assert!(run(&args(&base, path.clone())).await.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"broken json");
    task.abort();
}

#[tokio::test]
async fn concurrent_update_is_rejected_until_lock_is_released() {
    let (base, task) = mock(json!({"data":[{"id":"p/a"}]})).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, b"{}").unwrap();
    let lock = fs::File::create(dir.path().join("settings.json.update-models.lock")).unwrap();
    lock.lock().unwrap();
    assert!(run(&args(&base, path.clone())).await.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{}");
    drop(lock);
    assert!(run(&args(&base, path)).await.unwrap().changed);
    task.abort();
}
