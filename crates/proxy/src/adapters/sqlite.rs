use crate::domain::{Settings, SettingsStore};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::{path::Path, sync::Mutex};

pub struct SqliteStore(Mutex<Connection>);
impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .open(path)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let connection = Connection::open(path).context("No se pudo abrir SQLite")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        anyhow::ensure!(
            connection.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))? <= 1,
            "Esquema SQLite más reciente que este servidor"
        );
        connection.execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE; CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK (id=1), document TEXT NOT NULL); PRAGMA user_version=1; COMMIT;")?;
        let store = Self(Mutex::new(connection));
        // INSERT OR IGNORE persists a stable host identity on the first boot.
        store.0.lock().unwrap().execute(
            "INSERT OR IGNORE INTO settings VALUES (1, ?1)",
            [serde_json::to_string(&Settings::default())?],
        )?;
        Ok(store)
    }
}
impl SettingsStore for SqliteStore {
    fn load(&self) -> Result<Settings> {
        let raw: String = self.0.lock().unwrap().query_row(
            "SELECT document FROM settings WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&raw)?)
    }
    fn save(&self, settings: &Settings) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE settings SET document=?1 WHERE id=1",
            [serde_json::to_string(settings)?],
        )?;
        Ok(())
    }
}
