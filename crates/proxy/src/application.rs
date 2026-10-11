use crate::domain::{Settings, SettingsStore};
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub struct Application {
    store: Arc<dyn SettingsStore>,
    state: RwLock<(Settings, u64)>,
    pub mutation: tokio::sync::Mutex<()>,
    pub pending: tokio::sync::Mutex<HashMap<String, crate::domain::AuthorizationAttempt>>,
    pub port: u16,
}
impl Application {
    pub fn open(store: Arc<dyn SettingsStore>, port: u16) -> Result<Self> {
        let mut settings = store.load()?;
        let migrated = settings.separate_chatgpt();
        validate(&settings)?;
        if migrated {
            store.save(&settings)?;
        }
        Ok(Self {
            store,
            state: RwLock::new((settings, 0)),
            mutation: tokio::sync::Mutex::new(()),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            port,
        })
    }
    pub fn settings(&self) -> Settings {
        self.state.read().unwrap().0.clone()
    }
    pub fn snapshot(&self) -> (Settings, u64) {
        self.state.read().unwrap().clone()
    }
    // Callers serialize mutations, including OAuth rotation, with `mutation`.
    pub fn save(&self, settings: Settings) -> Result<()> {
        validate(&settings)?;
        self.store.save(&settings)?;
        let mut state = self.state.write().unwrap();
        let revision = state.1 + 1;
        *state = (settings, revision);
        Ok(())
    }
    pub async fn replace(&self, mut settings: Settings) -> Result<()> {
        let _guard = self.mutation.lock().await;
        let previous = self.settings();
        settings.host_id = previous.host_id;
        for p in &mut settings.providers {
            if let Some(old) = previous
                .providers
                .iter()
                .find(|old| old.id == p.id && old.kind == p.kind)
            {
                if p.api_key.is_none() {
                    p.api_key = old.api_key.clone();
                }
                // Browser updates never replace server-managed OAuth credentials.
                p.subscription = old.subscription.clone();
            } else {
                p.subscription = None;
            }
        }
        self.save(settings)
    }
}
pub(crate) fn validate_url(base: &str) -> Result<()> {
    let url = reqwest::Url::parse(base)?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "La URL debe ser HTTP(S), sin credenciales, query ni fragmento"
    );
    Ok(())
}
fn validate(settings: &Settings) -> Result<()> {
    settings.validate()?;
    if let Some(url) = &settings.public_url {
        validate_url(url)?;
    }
    for p in &settings.providers {
        validate_url(&p.base_url)?;
        if p.kind == crate::domain::ProviderKind::Google {
            ensure!(
                p.models
                    .iter()
                    .all(|m| m.protocol == crate::domain::Protocol::Chat),
                "Google usa Chat Completions"
            );
        }
    }
    Ok(())
}
