use anyhow::Result;
use claude_proxy_engine::diagnostics::{TracePolicy, observe, redact};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub struct Traces {
    connection: Arc<Mutex<Connection>>,
    sender: mpsc::SyncSender<Value>,
}
impl Traces {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS traces (id TEXT PRIMARY KEY, created INTEGER NOT NULL, expires INTEGER NOT NULL, document TEXT NOT NULL); CREATE INDEX IF NOT EXISTS traces_expiry ON traces(expires);")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let connection = Arc::new(Mutex::new(connection));
        let (sender, receiver) = mpsc::sync_channel::<Value>(64);
        let writer = connection.clone();
        std::thread::Builder::new().name("trace-writer".into()).spawn(move || {
            loop {
                let received = receiver.recv_timeout(Duration::from_secs(60));
                let connection = writer.lock().unwrap();
                let result = (|| -> Result<()> {
                    connection.execute("DELETE FROM traces WHERE expires <= ?1", [now()])?;
                    if let Ok(v) = &received {
                        connection.execute("INSERT OR REPLACE INTO traces VALUES (?1, ?2, ?3, ?4)", rusqlite::params![v["id"].as_str(), v["created"].as_u64(), v["expires"].as_u64(), v.to_string()])?;
                        connection.execute("DELETE FROM traces WHERE id NOT IN (SELECT id FROM traces ORDER BY created DESC LIMIT 2000)", [])?;
                    }
                    Ok(())
                })();
                if result.is_err() { tracing::warn!("No se pudo persistir una traza"); }
                if matches!(received, Err(mpsc::RecvTimeoutError::Disconnected)) { break; }
            }
        })?;
        Ok(Arc::new(Self { connection, sender }))
    }
    pub fn list(&self, id: Option<&str>) -> Result<Value> {
        let connection = self.connection.lock().unwrap();
        connection.execute("DELETE FROM traces WHERE expires <= ?1", [now()])?;
        let mut query = connection.prepare("SELECT document FROM traces WHERE (?1 IS NULL OR id=?1) ORDER BY created DESC LIMIT 100")?;
        let items = query
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(
            json!({"data":items.into_iter().map(|s| serde_json::from_str::<Value>(&s)).collect::<std::result::Result<Vec<_>,_>>()?}),
        )
    }
    pub fn page(&self, after: Option<&str>) -> Result<Value> {
        let connection = self.connection.lock().unwrap();
        connection.execute("DELETE FROM traces WHERE expires <= ?1", [now()])?;
        let mut query = connection.prepare("SELECT document FROM traces WHERE (?1 IS NULL OR created < (SELECT created FROM traces WHERE id=?1) OR (created=(SELECT created FROM traces WHERE id=?1) AND id < ?1)) ORDER BY created DESC, id DESC LIMIT 101")?;
        let mut items = query
            .query_map([after], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = items.len() > 100;
        items.truncate(100);
        let items = items
            .into_iter()
            .map(|s| serde_json::from_str::<Value>(&s))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!({"last_id":items.last().map(|v| &v["id"]),"data":items,"has_more":has_more}))
    }
    pub fn clear(&self) -> Result<()> {
        self.connection
            .lock()
            .unwrap()
            .execute("DELETE FROM traces", [])?;
        Ok(())
    }
    pub fn start(self: &Arc<Self>, policy: &TracePolicy) -> Option<Capture> {
        policy.enabled.then(|| Capture(Arc::new(Session {
            repository: self.clone(), policy: policy.clone(),
            record: Mutex::new(json!({"id":uuid::Uuid::new_v4().to_string(),"created":now(),"expires":now()+policy.retention_hours*3600,"visibility":"client-proxy-provider boundary only","outcome":"cancelled","stages":[],"dropped_events":0,"captured_bytes":0})),
        })))
    }
}
struct Session {
    repository: Arc<Traces>,
    policy: TracePolicy,
    record: Mutex<Value>,
}
#[derive(Clone)]
pub struct Capture(Arc<Session>);
impl Capture {
    pub fn id(&self) -> String {
        self.0.record.lock().unwrap()["id"].as_str().unwrap().into()
    }
    pub fn stage(&self, name: &str, value: &Value) {
        let mut record = self.0.record.lock().unwrap();
        let full = self.0.policy.full_until.is_some_and(|until| until > now());
        let mut entry = json!({"stage":name,"observation":observe(value)});
        entry["capture_mode"] = json!(if full { "full" } else { "metadata" });
        if full && value.to_string().len() > 128 * 1024 {
            entry["content_omitted"] = json!("stage_size_limit");
        }
        if full && value.to_string().len() <= 128 * 1024 {
            entry["content"] = redact(value);
        }
        let size = entry.to_string().len();
        let used = record["captured_bytes"].as_u64().unwrap_or(0) as usize;
        if record["stages"].as_array().unwrap().len() < 512 && used + size < 2 * 1024 * 1024 {
            record["stages"].as_array_mut().unwrap().push(entry);
            record["captured_bytes"] = json!(used + size);
        } else {
            let dropped = record["dropped_events"].as_u64().unwrap() + 1;
            record["dropped_events"] = json!(dropped);
        }
    }
    pub fn route(&self, model: &str, protocol: crate::domain::Protocol) {
        let mut record = self.0.record.lock().unwrap();
        record["model"] = json!(model);
        record["protocol"] = json!(protocol);
    }
    pub fn transport(&self, status: u16, elapsed_ms: u128) {
        let mut record = self.0.record.lock().unwrap();
        record["upstream_status"] = json!(status);
        record["headers_elapsed_ms"] = json!(elapsed_ms as u64);
    }
    pub fn comparison(&self, report: &Value) {
        self.0.record.lock().unwrap()["comparison"] = report.clone();
    }
    pub fn warnings(&self, warnings: &[&str]) {
        self.0.record.lock().unwrap()["expected_adaptations"] = json!(warnings);
    }
    pub fn finish(&self, outcome: &str) {
        let mut record = self.0.record.lock().unwrap();
        record["outcome"] = json!(outcome);
        record["assessment"] = json!(match outcome {
            "conversion_error" | "stream_error" => "conversion_or_stream_failure",
            "completed" => "completed_inspect_adaptations_and_observations",
            "upstream_error" | "transport_error" => "provider_or_transport_failure",
            _ => "incomplete_or_invalid_request",
        });
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let record = self.record.get_mut().unwrap().take();
        if self.repository.sender.try_send(record).is_err() {
            tracing::warn!("Cola de trazas llena; traza descartada");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_does_not_skip_or_repeat_traces_with_identical_timestamps() {
        let directory = tempfile::tempdir().unwrap();
        let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
        {
            let connection = traces.connection.lock().unwrap();
            for i in 0..105 {
                let id = format!("trace-{i:03}");
                let record = json!({"id":id,"created":now(),"expires":now()+3600});
                connection
                    .execute(
                        "INSERT INTO traces VALUES (?1,?2,?3,?4)",
                        rusqlite::params![id, now(), now() + 3600, record.to_string()],
                    )
                    .unwrap();
            }
        }
        let first = traces.page(None).unwrap();
        assert_eq!(first["data"].as_array().unwrap().len(), 100);
        assert_eq!(first["has_more"], true);
        let second = traces.page(first["last_id"].as_str()).unwrap();
        assert_eq!(second["data"].as_array().unwrap().len(), 5);
        assert_eq!(second["has_more"], false);
        assert!(
            !first["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == second["data"][0]["id"])
        );
    }
}
