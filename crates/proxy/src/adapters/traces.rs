use anyhow::Result;
use claude_proxy_engine::diagnostics::{
    TRACE_RETENTION_SECONDS, TracePolicy, differences, observe, redact,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub struct Traces {
    connection: Arc<Mutex<Connection>>,
    sender: mpsc::SyncSender<Write>,
}
enum Write {
    Record(Value),
    Flush(mpsc::Sender<()>),
}

fn purge(connection: &Connection) -> Result<()> {
    // Also cap legacy records which were written with different policies.
    connection.execute(
        "DELETE FROM traces WHERE expires <= ?1 OR created <= ?2",
        rusqlite::params![now(), now().saturating_sub(TRACE_RETENTION_SECONDS)],
    )?;
    Ok(())
}
fn persist(connection: &Connection, record: &Value) -> Result<()> {
    purge(connection)?;
    connection.execute(
        "INSERT OR REPLACE INTO traces VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            record["id"].as_str(),
            record["created"].as_u64(),
            record["expires"].as_u64(),
            record.to_string()
        ],
    )?;
    purge(connection)?;
    Ok(())
}
impl Traces {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS traces (id TEXT PRIMARY KEY, created INTEGER NOT NULL, expires INTEGER NOT NULL, document TEXT NOT NULL); CREATE INDEX IF NOT EXISTS traces_expiry ON traces(expires); CREATE INDEX IF NOT EXISTS traces_created ON traces(created, id);")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        purge(&connection)?;
        let connection = Arc::new(Mutex::new(connection));
        let (sender, receiver) = mpsc::sync_channel::<Write>(64);
        let writer = connection.clone();
        std::thread::Builder::new()
            .name("trace-writer".into())
            .spawn(move || {
                loop {
                    let received = receiver.recv_timeout(Duration::from_secs(60));
                    let connection = writer.lock().unwrap();
                    let result = match &received {
                        Ok(Write::Record(record)) => persist(&connection, record),
                        Ok(Write::Flush(sender)) => {
                            let _ = sender.send(());
                            Ok(())
                        }
                        Err(_) => purge(&connection),
                    };
                    if result.is_err() {
                        tracing::warn!("No se pudo persistir una traza");
                    }
                    if matches!(received, Err(mpsc::RecvTimeoutError::Disconnected)) {
                        break;
                    }
                }
            })?;
        Ok(Arc::new(Self { connection, sender }))
    }
    /// Drain queued writes before shutdown or a read-after-write operation.
    pub fn flush(&self) -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        self.sender.send(Write::Flush(sender))?;
        receiver.recv()?;
        Ok(())
    }
    fn save(&self, record: Value) -> Result<()> {
        match self.sender.try_send(Write::Record(record)) {
            Ok(()) => Ok(()),
            // Backpressure persists synchronously instead of dropping a trace or
            // allowing an unbounded queue to exhaust memory.
            Err(mpsc::TrySendError::Full(Write::Record(record))) => {
                persist(&self.connection.lock().unwrap(), &record)
            }
            Err(_) => anyhow::bail!("El escritor de trazas se ha detenido"),
        }
    }
    pub fn list(&self, id: Option<&str>) -> Result<Value> {
        let connection = self.connection.lock().unwrap();
        purge(&connection)?;
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
        purge(&connection)?;
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
        self.flush()?;
        self.connection
            .lock()
            .unwrap()
            .execute("DELETE FROM traces", [])?;
        Ok(())
    }
    pub fn start(self: &Arc<Self>, policy: &TracePolicy) -> Option<Capture> {
        let created = now();
        Some(Capture(Arc::new(Session {
            repository: self.clone(),
            policy: policy.clone(),
            started: Instant::now(),
            record: Mutex::new(
                json!({"id":uuid::Uuid::new_v4().to_string(),"created":created,"expires":created+TRACE_RETENTION_SECONDS,"visibility":"client-proxy-provider boundary only","debug":policy.debug,"outcome":"cancelled","stages":[],"dropped_events":0,"captured_bytes":0}),
            ),
        })))
    }
}
struct Session {
    repository: Arc<Traces>,
    policy: TracePolicy,
    started: Instant,
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
        // Error details remain useful in normal mode; request/response content
        // from successful stages is included only in debug mode.
        let full = self.0.policy.debug || value["type"] == "error" || value.get("error").is_some();
        let mut entry = json!({"stage":name,"sequence":record["stages"].as_array().unwrap().len(),"observation":observe(value)});
        entry["elapsed_us"] = json!(self.0.started.elapsed().as_micros() as u64);
        entry["capture_mode"] = json!(if full { "full" } else { "metadata" });
        if full {
            entry["content"] = redact(value);
        }
        entry["boundary"] = json!(match name {
            "client_request" => "claude_to_proxy",
            "upstream_request" => "proxy_to_provider",
            "upstream_response" | "upstream_event" => "provider_to_proxy",
            "client_response" | "client_event" => "proxy_to_claude",
            _ => "proxy_internal",
        });
        let source = match name {
            "upstream_request" => Some("client_request"),
            "client_response" => Some("upstream_response"),
            _ => None,
        };
        if full
            && let Some(source) = source
            && let Some(previous) = record["stages"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|stage| stage["stage"] == source && stage.get("content").is_some())
            && let Some(content) = entry.get("content")
        {
            entry["changes"] = differences(&previous["content"], content);
            entry["compared_to"] = json!(source);
        }
        let size = entry.to_string().len();
        let used = record["captured_bytes"].as_u64().unwrap_or(0) as usize;
        if self.0.policy.debug
            || (record["stages"].as_array().unwrap().len() < 512 && used + size < 2 * 1024 * 1024)
        {
            record["stages"].as_array_mut().unwrap().push(entry);
            record["captured_bytes"] = json!(used + size);
        } else {
            let dropped = record["dropped_events"].as_u64().unwrap() + 1;
            record["dropped_events"] = json!(dropped);
        }
    }
    pub fn omitted_stage(&self, name: &str, reason: &str) {
        self.stage(name, &Value::Null);
        let mut record = self.0.record.lock().unwrap();
        let entry = record["stages"].as_array_mut().unwrap().last_mut().unwrap();
        let before = entry.to_string().len();
        entry.as_object_mut().unwrap().remove("content");
        entry["content_omitted"] = json!(reason);
        let after = entry.to_string().len();
        record["captured_bytes"] =
            json!(record["captured_bytes"].as_u64().unwrap() - before as u64 + after as u64);
    }
    pub fn operation(&self, operation: &str) {
        self.0.record.lock().unwrap()["operation"] = json!(operation);
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
        let outcome = if outcome == "completed"
            && record["comparison"]["confirmed_losses"]
                .as_array()
                .is_some_and(|losses| !losses.is_empty())
        {
            "data_loss"
        } else {
            outcome
        };
        record["outcome"] = json!(outcome);
        record["assessment"] = json!(match outcome {
            "conversion_error" | "stream_error" | "data_loss" => "conversion_or_stream_failure",
            "completed" => "completed_inspect_adaptations_and_observations",
            "upstream_error" | "transport_error" => "provider_or_transport_failure",
            _ => "incomplete_or_invalid_request",
        });
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let record = self.record.get_mut().unwrap().take();
        if !self.policy.debug && record["outcome"] == "completed" {
            return;
        }
        if self.repository.save(record).is_err() {
            tracing::warn!("No se pudo enviar la traza al escritor");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retention_purges_on_open_and_read_without_a_record_count_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("traces.db");
        let traces = Traces::open(&path).unwrap();
        {
            let connection = traces.connection.lock().unwrap();
            for (id, created, expires) in [
                ("old-legacy", now() - TRACE_RETENTION_SECONDS, now() + 3600),
                ("expired", now() - 3600, now() - 1),
                ("recent", now() - TRACE_RETENTION_SECONDS + 60, now() + 60),
            ] {
                connection
                    .execute(
                        "INSERT INTO traces VALUES (?1,?2,?3,?4)",
                        rusqlite::params![id, created, expires, json!({"id":id}).to_string()],
                    )
                    .unwrap();
            }
        }
        let reopened = Traces::open(&path).unwrap();
        assert_eq!(
            reopened.list(None).unwrap()["data"],
            json!([{"id":"recent"}])
        );
        {
            let connection = reopened.connection.lock().unwrap();
            for i in 0..2005 {
                let id = format!("new-{i}");
                persist(
                    &connection,
                    &json!({"id":id,"created":now(),"expires":now()+TRACE_RETENTION_SECONDS}),
                )
                .unwrap();
            }
            let count: i64 = connection
                .query_row("SELECT COUNT(*) FROM traces", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 2006);
        }
    }

    #[test]
    fn saturated_writer_persists_instead_of_dropping_and_clear_drains_pending_records() {
        let connection = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        connection.lock().unwrap().execute_batch("CREATE TABLE traces (id TEXT PRIMARY KEY, created INTEGER, expires INTEGER, document TEXT);").unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(Write::Record(json!({}))).unwrap();
        let traces = Traces {
            connection: connection.clone(),
            sender,
        };
        traces
            .save(json!({"id":"overflow","created":now(),"expires":now()+TRACE_RETENTION_SECONDS}))
            .unwrap();
        let count: i64 = connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM traces", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(receiver);
        let directory = tempfile::tempdir().unwrap();
        let traces = Traces::open(&directory.path().join("traces.db")).unwrap();
        let capture = traces.start(&TracePolicy { debug: true }).unwrap();
        capture.finish("completed");
        drop(capture);
        traces.clear().unwrap();
        assert_eq!(traces.list(None).unwrap()["data"], json!([]));
    }

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
