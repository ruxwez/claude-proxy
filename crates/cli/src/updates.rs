//! Stable release discovery shared by notices and the local server updater.
use anyhow::{Context, Result, ensure};
use semver::Version;
use std::time::Duration;

pub const RELEASES_URL: &str = "https://github.com/ruxwez/claude-proxy/releases/latest";

pub async fn latest() -> Result<Version> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .https_only(true)
        .user_agent(concat!("claude-proxy-cli/", env!("CARGO_PKG_VERSION")))
        .build()?;
    discover(&http, RELEASES_URL).await
}

async fn discover(http: &reqwest::Client, endpoint: &str) -> Result<Version> {
    let source = reqwest::Url::parse(endpoint)?;
    let release = http.head(source.clone()).send().await?.error_for_status()?;
    ensure!(
        release.url().origin() == source.origin(),
        "Origen de release inválido"
    );
    version_from_path(release.url().path())
}

fn version_from_path(path: &str) -> Result<Version> {
    let tag = path
        .strip_prefix("/ruxwez/claude-proxy/releases/tag/v")
        .context("Release sin tag de versión")?;
    let version = Version::parse(tag)?;
    ensure!(version.pre.is_empty(), "La release no es estable");
    Ok(version)
}

pub fn notices(latest: &Version, cli: &str, server: Option<&str>) -> Vec<String> {
    [("CLI", Some(cli)), ("servidor proxy", server)]
        .into_iter()
        .filter_map(|(component, current)| {
            let current = Version::parse(current?).ok()?;
            (latest > &current)
                .then(|| format!("Nueva versión del {component}: {current} → {latest}"))
        })
        .collect()
}

pub async fn print_notices(server: Option<&str>) {
    if let Ok(latest) = latest().await {
        for notice in notices(&latest, env!("CARGO_PKG_VERSION"), server) {
            eprintln!("{notice}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_cli_and_server_independently_without_downgrades() {
        let latest = Version::parse("1.2.0").unwrap();
        assert_eq!(
            notices(&latest, "1.1.0", Some("1.2.0")),
            ["Nueva versión del CLI: 1.1.0 → 1.2.0"]
        );
        assert_eq!(
            notices(&latest, "1.3.0", Some("1.0.0")),
            ["Nueva versión del servidor proxy: 1.0.0 → 1.2.0"]
        );
        assert_eq!(notices(&latest, "1.0.0", Some("1.1.0")).len(), 2);
        assert!(notices(&latest, "1.2.0", Some("1.3.0")).is_empty());
        assert_eq!(notices(&latest, "1.1.0", Some("unknown")).len(), 1);
    }
    #[tokio::test]
    async fn discovers_redirected_stable_release_and_rejects_missing_or_unstable_tags() {
        use axum::{Router, response::Redirect, routing::head};
        let router = Router::new()
            .route(
                "/latest",
                head(|| async { Redirect::temporary("/ruxwez/claude-proxy/releases/tag/v1.2.3") }),
            )
            .route(
                "/unstable",
                head(|| async {
                    Redirect::temporary("/ruxwez/claude-proxy/releases/tag/v1.3.0-beta.1")
                }),
            )
            .route(
                "/missing",
                head(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .route(
                "/ruxwez/claude-proxy/releases/tag/{tag}",
                head(|| async { axum::http::StatusCode::OK }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            discover(&http, &format!("{endpoint}/latest"))
                .await
                .unwrap(),
            Version::parse("1.2.3").unwrap()
        );
        assert!(
            discover(&http, &format!("{endpoint}/unstable"))
                .await
                .is_err()
        );
        assert!(
            discover(&http, &format!("{endpoint}/missing"))
                .await
                .is_err()
        );
        task.abort();
    }
}
