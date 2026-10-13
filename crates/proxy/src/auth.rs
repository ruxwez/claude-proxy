//! One shared gateway key using Claude Code's native headers.
use axum::http::{HeaderMap, header};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone, Default)]
pub struct Auth(Option<[u8; 32]>);
impl Auth {
    pub fn from_env() -> anyhow::Result<Self> {
        match std::env::var("AUTH_KEY") {
            Ok(key) => {
                anyhow::ensure!(
                    !key.is_empty() && !key.chars().any(char::is_control),
                    "AUTH_KEY debe ser una clave no vacía sin caracteres de control"
                );
                Ok(Self::new(Some(&key)))
            }
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(_) => anyhow::bail!("AUTH_KEY debe ser UTF-8"),
        }
    }
    pub fn new(key: Option<&str>) -> Self {
        Self(key.map(|key| Sha256::digest(key.as_bytes()).into()))
    }
    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }
    pub fn accepts(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.0 else {
            return true;
        };
        // An explicit Authorization header takes precedence; never bypass a bad token.
        let received = if let Some(value) = headers.get(header::AUTHORIZATION) {
            value
                .to_str()
                .ok()
                .and_then(|s| s.split_once(' '))
                .and_then(|(scheme, key)| scheme.eq_ignore_ascii_case("bearer").then_some(key))
        } else {
            headers.get("x-api-key").and_then(|s| s.to_str().ok())
        };
        let Some(received) = received else {
            return false;
        };
        let digest: [u8; 32] = Sha256::digest(received.as_bytes()).into();
        bool::from(expected.ct_eq(&digest))
    }
}
