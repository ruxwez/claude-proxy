//! Infrastructure selection; credentials and the DSN stay on the server.
use crate::domain::{Settings, SettingsStore};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{
    Row,
    any::{AnyPoolOptions, AnyRow},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum Database {
    Sqlite { path: PathBuf },
    Postgres { url: String },
    Mysql { url: String },
}
impl Database {
    pub fn open(&self) -> Result<Arc<dyn SettingsStore>> {
        match self {
            Self::Sqlite { path } => Ok(Arc::new(crate::sqlite::SqliteStore::open(path)?)),
            Self::Postgres { url } => {
                ensure!(
                    url.starts_with("postgres://") || url.starts_with("postgresql://"),
                    "DSN PostgreSQL inválido"
                );
                Ok(Arc::new(RemoteStore::open(url, true)?))
            }
            Self::Mysql { url } => {
                ensure!(url.starts_with("mysql://"), "DSN MySQL inválido");
                Ok(Arc::new(RemoteStore::open(url, false)?))
            }
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("Falta directorio")?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        serde_json::to_writer_pretty(&mut temp, self)?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}
fn document(row: AnyRow) -> Result<String> {
    match row.try_get::<String, _>(0) {
        Ok(document) => Ok(document),
        // MySQL exposes LONGTEXT as BLOB through SQLx Any; decode UTF-8 explicitly.
        Err(_) => Ok(String::from_utf8(row.try_get::<Vec<u8>, _>(0)?)?),
    }
}
type Reply = mpsc::SyncSender<Result<String>>;
enum Command {
    Load(Reply),
    Save(String, Reply),
}
struct RemoteStore {
    sender: mpsc::Sender<Command>,
}
impl RemoteStore {
    fn open(url: &str, postgres: bool) -> Result<Self> {
        let url = url.to_string();
        let (sender, commands) = mpsc::channel();
        let (ready, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("settings-database".into())
            .spawn(move || run_database(url, postgres, commands, ready))?;
        result.recv()??;
        Ok(Self { sender })
    }
}
fn run_database(
    url: String,
    postgres: bool,
    commands: mpsc::Receiver<Command>,
    ready: mpsc::SyncSender<Result<()>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error.into()));
            return;
        }
    };
    runtime.block_on(async {
        sqlx::any::install_default_drivers();
        let pool = match initialize(&url, postgres).await {
            Ok(pool) => { let _ = ready.send(Ok(())); pool }
            Err(_) => { let _ = ready.send(Err(anyhow::anyhow!("No se pudo inicializar la base de datos remota; comprueba conexión, TLS y permisos"))); return; }
        };
        while let Ok(command) = commands.recv() {
            match command {
                Command::Load(reply) => {
                    let result = tokio::time::timeout(std::time::Duration::from_secs(15), sqlx::query("SELECT document FROM settings WHERE id=1").fetch_one(&pool)).await;
                    let result = result.map_err(anyhow::Error::from).and_then(|row| row.map_err(Into::into)).and_then(document);
                    let _ = reply.send(result);
                }
                Command::Save(document, reply) => {
                    let sql = if postgres { "UPDATE settings SET document=$1 WHERE id=1" } else { "UPDATE settings SET document=? WHERE id=1" };
                    let result = tokio::time::timeout(std::time::Duration::from_secs(15), sqlx::query(sql).bind(document).execute(&pool)).await;
                    let result = result.map_err(anyhow::Error::from).and_then(|rows| rows.map_err(Into::into)).map(|_| String::new());
                    let _ = reply.send(result);
                }
            }
        }
    });
}
async fn initialize(url: &str, postgres: bool) -> Result<sqlx::AnyPool> {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let pool = AnyPoolOptions::new().max_connections(1).acquire_timeout(std::time::Duration::from_secs(15)).connect(url).await?;
        let schema = if postgres {
            "CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY, document TEXT NOT NULL)"
        } else {
            "CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY, document LONGTEXT NOT NULL)"
        };
        sqlx::query(schema).execute(&pool).await?;
        let insert = if postgres {
            "INSERT INTO settings (id,document) VALUES (1,$1) ON CONFLICT (id) DO NOTHING"
        } else {
            "INSERT IGNORE INTO settings (id,document) VALUES (1,?)"
        };
        sqlx::query(insert).bind(serde_json::to_string(&Settings::default())?).execute(&pool).await?;
        anyhow::Ok(pool)
    }).await?
}
impl SettingsStore for RemoteStore {
    fn load(&self) -> Result<Settings> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender.send(Command::Load(reply))?;
        Ok(serde_json::from_str(&result.recv()??)?)
    }
    fn save(&self, settings: &Settings) -> Result<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Save(serde_json::to_string(settings)?, reply))?;
        result.recv()??;
        Ok(())
    }
}
