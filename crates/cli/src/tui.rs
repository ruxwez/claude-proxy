use crate::control::Client;
use anyhow::{Context, Result, ensure};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::{Value, json};
use std::{
    io::IsTerminal,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Field {
    label: &'static str,
    value: String,
    secret: bool,
}
impl Field {
    fn new(label: &'static str, value: impl ToString) -> Self {
        Self {
            label,
            value: value.to_string(),
            secret: false,
        }
    }
}
enum Action {
    Provider(Option<usize>),
    Model(Option<usize>),
    Preferences,
    Setup,
    TracePolicy,
    DeleteProvider(usize),
    ClearTraces,
}
struct Form {
    title: &'static str,
    fields: Vec<Field>,
    selected: usize,
    action: Action,
}
struct Panel {
    client: Client,
    settings: Value,
    catalog: Vec<Value>,
    traces: Vec<Value>,
    tab: usize,
    selected: usize,
    provider: usize,
    filter: String,
    filtering: bool,
    form: Option<Form>,
    detail: Option<String>,
    scroll: u16,
    message: String,
    configured: bool,
    trace_cursors: Vec<String>,
}
impl Panel {
    async fn load(client: Client) -> Result<Self> {
        let status = client.get("/api/status").await?;
        let settings = client.get("/api/settings").await?;
        let configured = status["configured"] == true;
        let mut panel = Self {
            client,
            settings,
            catalog: vec![],
            traces: vec![],
            tab: 0,
            selected: 0,
            provider: 0,
            filter: String::new(),
            filtering: false,
            form: None,
            detail: None,
            scroll: 0,
            message: String::new(),
            configured,
            trace_cursors: vec![],
        };
        if !configured {
            panel.form = Some(Form {
                title: "Configurar servidor",
                fields: vec![
                    Field::new("Backend: sqlite / postgres / mysql", "sqlite"),
                    Field::new("Ruta SQLite (vacío: directorio del servidor) o DSN", ""),
                ],
                selected: 0,
                action: Action::Setup,
            });
            panel.form.as_mut().unwrap().fields[1].secret = true;
        }
        Ok(panel)
    }
    fn providers(&self) -> &[Value] {
        self.settings["providers"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    fn current_provider(&self) -> Result<&Value> {
        self.providers()
            .get(self.provider)
            .context("Selecciona un proveedor")
    }
    fn model_rows(&self) -> Vec<(Value, Option<usize>)> {
        let Some(provider) = self.providers().get(self.provider) else {
            return vec![];
        };
        let saved = provider["models"].as_array().cloned().unwrap_or_default();
        let mut rows: Vec<_> = saved
            .iter()
            .enumerate()
            .map(|(i, m)| (m.clone(), Some(i)))
            .collect();
        for model in &self.catalog {
            if !saved.iter().any(|s| s["id"] == model["id"]) {
                let mut model = model.clone();
                model["enabled"] = json!(false);
                rows.push((model, None));
            }
        }
        rows.retain(|(m, _)| {
            format!("{} {}", m["id"], m["display_name"])
                .to_lowercase()
                .contains(&self.filter.to_lowercase())
        });
        rows.sort_by(|a, b| a.0["id"].as_str().cmp(&b.0["id"].as_str()));
        rows
    }
    fn length(&self) -> usize {
        match self.tab {
            0 => self.providers().len(),
            1 => self.model_rows().len(),
            3 => self.traces.len(),
            _ => 1,
        }
    }
    fn trace_path(&self) -> String {
        self.trace_cursors
            .last()
            .map(|id| format!("/api/traces?after={id}"))
            .unwrap_or_else(|| "/api/traces".into())
    }
    async fn refresh(&mut self) -> Result<()> {
        self.settings = self.client.get("/api/settings").await?;
        self.provider = self.provider.min(self.providers().len().saturating_sub(1));
        if self.tab == 1 {
            self.discover(false).await?;
        }
        if self.tab == 3 {
            self.traces = self.client.get(&self.trace_path()).await?["data"]
                .as_array()
                .cloned()
                .unwrap_or_default();
        }
        Ok(())
    }
    async fn discover(&mut self, refresh: bool) -> Result<()> {
        let provider = self.current_provider()?.clone();
        let result = self
            .client
            .request(
                reqwest::Method::POST,
                "/api/providers/catalog",
                Some(&json!({"provider":provider,"refresh":refresh})),
            )
            .await?;
        self.catalog = result["data"].as_array().cloned().unwrap_or_default();
        self.message = result["warning"]
            .as_str()
            .unwrap_or("Catálogo actualizado")
            .into();
        Ok(())
    }
    fn edit_provider(&mut self, index: Option<usize>) {
        let p = index
            .and_then(|i| self.providers().get(i))
            .cloned()
            .unwrap_or(json!({"kind":"openai_compatible","enabled":true}));
        let mut fields = vec![
            Field::new("ID", text(&p, "id")),
            Field::new("Nombre", text(&p, "name")),
            Field::new(
                "Tipo: opencode_go / openai / chatgpt / google / openai_compatible",
                text(&p, "kind"),
            ),
            Field::new(
                "Endpoint (vacío: predeterminado del tipo)",
                text(&p, "base_url"),
            ),
            Field::new("API key (vacío: conservar)", ""),
            Field::new(
                "Proveedor de models.dev (opcional)",
                text(&p, "models_dev_provider"),
            ),
            Field::new(
                "Habilitado: true / false",
                p["enabled"].as_bool().unwrap_or(true),
            ),
        ];
        fields[4].secret = true;
        self.form = Some(Form {
            title: "Conexión de proveedor",
            fields,
            selected: 0,
            action: Action::Provider(index),
        });
    }
    fn edit_model(&mut self, model: Value, index: Option<usize>) {
        self.form = Some(Form {
            title: "Modelo",
            fields: vec![
                Field::new("ID", text(&model, "id")),
                Field::new("Nombre", text(&model, "display_name")),
                Field::new(
                    "Protocolo: chat / responses / messages",
                    model["protocol"].as_str().unwrap_or(
                        if self
                            .providers()
                            .get(self.provider)
                            .is_some_and(|p| p["kind"] == "chatgpt")
                        {
                            "responses"
                        } else {
                            "chat"
                        },
                    ),
                ),
                Field::new(
                    "Habilitado: true / false",
                    model["enabled"].as_bool().unwrap_or(true),
                ),
            ],
            selected: 0,
            action: Action::Model(index),
        });
    }
    async fn submit(&mut self) -> Result<()> {
        let form = self.form.as_ref().context("Formulario ausente")?;
        let values: Vec<_> = form
            .fields
            .iter()
            .map(|f| f.value.trim().to_string())
            .collect();
        let boolean =
            |i: usize| -> Result<bool> { values[i].parse::<bool>().context("Usa true o false") };
        let optional_number = |i: usize| -> Result<Value> {
            if values[i].is_empty() {
                Ok(Value::Null)
            } else {
                Ok(json!(values[i].parse::<u64>()?))
            }
        };
        let mut next = self.settings.clone();
        match form.action {
            Action::Provider(index) => {
                let kind = values[2].as_str();
                ensure!(
                    [
                        "opencode_go",
                        "openai",
                        "chatgpt",
                        "google",
                        "openai_compatible"
                    ]
                    .contains(&kind),
                    "Tipo de proveedor inválido"
                );
                let base = if values[3].is_empty() {
                    match kind {
                        "opencode_go" => "https://opencode.ai/zen/go/v1",
                        "openai" | "chatgpt" => "https://api.openai.com/v1",
                        "google" => "https://generativelanguage.googleapis.com/v1beta/openai",
                        _ => anyhow::bail!("Indica el endpoint"),
                    }
                } else {
                    &values[3]
                };
                let mut provider = index
                    .map(|i| next["providers"][i].clone())
                    .unwrap_or(json!({"models":[]}));
                provider["id"] = json!(values[0]);
                provider["name"] = json!(values[1]);
                provider["kind"] = json!(kind);
                provider["base_url"] = json!(base);
                provider["api_key"] = if values[4].is_empty() {
                    Value::Null
                } else {
                    json!(values[4])
                };
                provider["models_dev_provider"] = if values[5].is_empty() {
                    Value::Null
                } else {
                    json!(values[5])
                };
                provider["enabled"] = json!(boolean(6)?);
                if let Some(i) = index {
                    next["providers"][i] = provider;
                } else {
                    next["providers"].as_array_mut().unwrap().push(provider);
                }
            }
            Action::Model(index) => {
                ensure!(
                    ["chat", "responses", "messages"].contains(&values[2].as_str()),
                    "Protocolo inválido"
                );
                let credential = if self.current_provider()?["kind"] == "chatgpt" {
                    "subscription"
                } else {
                    "api_key"
                };
                let model = json!({"id":values[0],"display_name":values[1],"protocol":values[2],"enabled":boolean(3)?,"credential":credential});
                if let Some(i) = index {
                    next["providers"][self.provider]["models"][i] = model;
                } else {
                    next["providers"][self.provider]["models"]
                        .as_array_mut()
                        .unwrap()
                        .push(model);
                }
            }
            Action::Preferences => {
                next["default_model"] = if values[0].is_empty() {
                    Value::Null
                } else {
                    json!(values[0])
                };
                next["context_tokens"] = optional_number(1)?;
                next["compact_tokens"] = optional_number(2)?;
                next["timeout_seconds"] = json!(values[3].parse::<u64>()?);
                next["public_url"] = if values[4].is_empty() {
                    Value::Null
                } else {
                    json!(values[4])
                };
            }
            Action::TracePolicy => {
                let enabled = boolean(0)?;
                let hours = values[1].parse::<u64>()?;
                let until = if values[2] == "CAPTURAR CONTENIDO" {
                    Some(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 3600)
                } else {
                    ensure!(
                        values[2].is_empty(),
                        "Escribe exactamente CAPTURAR CONTENIDO o deja vacío"
                    );
                    None
                };
                next["traces"] =
                    json!({"enabled":enabled,"retention_hours":hours,"full_until":until});
            }
            Action::Setup => {
                let body = match values[0].as_str() {
                    "sqlite" => json!({"backend":"sqlite","path":values[1]}),
                    "postgres" | "mysql" => json!({"backend":values[0],"url":values[1]}),
                    _ => anyhow::bail!("Backend inválido"),
                };
                self.client
                    .request(reqwest::Method::POST, "/api/setup", Some(&body))
                    .await?;
                self.configured = true;
                self.form = None;
                self.refresh().await?;
                return Ok(());
            }
            Action::DeleteProvider(index) => {
                ensure!(values[0] == "ELIMINAR", "Escribe ELIMINAR");
                next["providers"].as_array_mut().unwrap().remove(index);
            }
            Action::ClearTraces => {
                ensure!(values[0] == "ELIMINAR", "Escribe ELIMINAR");
                self.client
                    .request(reqwest::Method::DELETE, "/api/traces", None)
                    .await?;
                self.trace_cursors.clear();
                self.form = None;
                self.refresh().await?;
                return Ok(());
            }
        }
        self.client.put_settings(&next).await?;
        self.form = None;
        self.refresh().await?;
        self.message = "Guardado en el servidor".into();
        Ok(())
    }
    async fn action(&mut self, key: KeyCode, args: &crate::Args) -> Result<bool> {
        if let Some(form) = &mut self.form {
            match key {
                KeyCode::Esc if self.configured => self.form = None,
                KeyCode::Tab | KeyCode::Down => {
                    form.selected = (form.selected + 1) % form.fields.len()
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.selected = (form.selected + form.fields.len() - 1) % form.fields.len()
                }
                KeyCode::Char(c) => form.fields[form.selected].value.push(c),
                KeyCode::Backspace => {
                    form.fields[form.selected].value.pop();
                }
                KeyCode::Enter => self.submit().await?,
                _ => {}
            }
            return Ok(false);
        }
        if self.detail.is_some() {
            match key {
                KeyCode::Esc | KeyCode::Enter => {
                    self.detail = None;
                    self.scroll = 0;
                }
                KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(15),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(15),
                _ => {}
            }
            return Ok(false);
        }
        if self.filtering {
            match key {
                KeyCode::Esc | KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.selected = 0;
            return Ok(false);
        }
        match key {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Tab | KeyCode::Right => {
                self.tab = (self.tab + 1) % 4;
                self.selected = 0;
                if self.tab == 3 {
                    self.refresh().await?;
                }
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.tab = (self.tab + 3) % 4;
                self.selected = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.length().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('r') => {
                self.refresh().await?;
                if self.tab == 1 {
                    self.discover(true).await?;
                }
            }
            KeyCode::Char('/') if self.tab == 1 => self.filtering = true,
            KeyCode::Char('n') if self.tab == 0 => self.edit_provider(None),
            KeyCode::Char('n') if self.tab == 1 => self.edit_model(json!({}), None),
            KeyCode::Enter if self.tab == 0 && self.selected < self.providers().len() => {
                self.provider = self.selected;
                self.tab = 1;
                self.selected = 0;
                self.catalog.clear();
                self.discover(false).await?;
            }
            KeyCode::Char('e') if self.tab == 0 && self.selected < self.providers().len() => {
                self.edit_provider(Some(self.selected))
            }
            KeyCode::Char('e') if self.tab == 1 => {
                if let Some((model, index)) = self.model_rows().get(self.selected).cloned() {
                    self.edit_model(model, index);
                }
            }
            KeyCode::Enter if self.tab == 1 => {
                if let Some((model, _)) = self.model_rows().get(self.selected) {
                    let catalog = self.catalog.iter().find(|m| m["id"] == model["id"]);
                    self.detail = Some(serde_json::to_string_pretty(
                        &json!({"configured":model,"catalog":catalog}),
                    )?);
                }
            }
            KeyCode::Char(' ') if self.tab == 0 && self.selected < self.providers().len() => {
                let mut next = self.settings.clone();
                next["providers"][self.selected]["enabled"] =
                    json!(next["providers"][self.selected]["enabled"] != true);
                self.client.put_settings(&next).await?;
                self.refresh().await?;
            }
            KeyCode::Char('f') if self.tab == 1 => {
                if let Some((model, Some(_))) = self.model_rows().get(self.selected) {
                    let mut next = self.settings.clone();
                    next["default_model"] = json!(format!(
                        "{}/{}",
                        text(self.current_provider()?, "id"),
                        text(model, "id")
                    ));
                    self.client.put_settings(&next).await?;
                    self.refresh().await?;
                }
            }
            KeyCode::Char('m') if self.tab == 0 => {
                self.detail = Some(serde_json::to_string_pretty(
                    &self.client.get("/api/metadata/providers").await?,
                )?);
            }
            KeyCode::Char(' ') if self.tab == 1 => {
                if let Some((mut model, index)) = self.model_rows().get(self.selected).cloned() {
                    if let Some(index) = index {
                        let mut next = self.settings.clone();
                        model["enabled"] = json!(!model["enabled"].as_bool().unwrap_or(false));
                        next["providers"][self.provider]["models"][index] = model;
                        self.client.put_settings(&next).await?;
                        self.settings = self.client.get("/api/settings").await?;
                    } else {
                        model["enabled"] = json!(true);
                        self.edit_model(model, None);
                    }
                }
            }
            KeyCode::Char('d') if self.tab == 0 && self.selected < self.providers().len() => {
                self.form = Some(Form {
                    title: "Eliminar proveedor y modelos",
                    fields: vec![Field::new("Escribe ELIMINAR", "")],
                    selected: 0,
                    action: Action::DeleteProvider(self.selected),
                })
            }
            KeyCode::Char('d') if self.tab == 1 => {
                if let Some((_, Some(index))) = self.model_rows().get(self.selected).cloned() {
                    let mut next = self.settings.clone();
                    next["providers"][self.provider]["models"]
                        .as_array_mut()
                        .unwrap()
                        .remove(index);
                    self.client.put_settings(&next).await?;
                    self.settings = self.client.get("/api/settings").await?;
                }
            }
            KeyCode::Char('o') if self.tab == 0 && self.selected < self.providers().len() => {
                let id = text(&self.providers()[self.selected], "id");
                let result = self
                    .client
                    .request(
                        reqwest::Method::POST,
                        &format!("/api/providers/{id}/oauth"),
                        Some(&json!({})),
                    )
                    .await?;
                let url = result["url"].as_str().context("Falta URL OAuth")?;
                let _ = webbrowser::open(url);
                self.detail = Some(format!(
                    "Completa la conexión en el navegador y pulsa r al volver.\n\n{url}"
                ));
            }
            KeyCode::Char('u') | KeyCode::Char('K')
                if self.tab == 0 && self.selected < self.providers().len() =>
            {
                let id = text(&self.providers()[self.selected], "id");
                let resource = if key == KeyCode::Char('K') {
                    "api-key"
                } else {
                    "oauth"
                };
                let result = self
                    .client
                    .request(
                        reqwest::Method::DELETE,
                        &format!("/api/providers/{id}/{resource}"),
                        None,
                    )
                    .await?;
                self.message = if result["remote_revoked"] == false {
                    "Tokens locales eliminados; revocación remota no confirmada"
                } else {
                    "Credencial eliminada"
                }
                .into();
                self.refresh().await?;
            }
            KeyCode::Char('e') if self.tab == 2 => {
                self.form = Some(Form {
                    title: "Preferencias",
                    fields: vec![
                        Field::new(
                            "Modelo predeterminado (proveedor/modelo)",
                            text(&self.settings, "default_model"),
                        ),
                        Field::new(
                            "Contexto (vacío: Claude)",
                            number(&self.settings, "context_tokens"),
                        ),
                        Field::new(
                            "Compactación (vacío: Claude)",
                            number(&self.settings, "compact_tokens"),
                        ),
                        Field::new(
                            "Timeout (segundos)",
                            number(&self.settings, "timeout_seconds"),
                        ),
                        Field::new(
                            "URL pública Anthropic (vacío: automática)",
                            text(&self.settings, "public_url"),
                        ),
                    ],
                    selected: 0,
                    action: Action::Preferences,
                })
            }
            KeyCode::Char('R') if self.tab == 2 => {
                let mut next = self.settings.clone();
                next["context_tokens"] = json!(990000);
                next["compact_tokens"] = json!(900000);
                self.client.put_settings(&next).await?;
                self.refresh().await?;
            }
            KeyCode::Char('s') => {
                let mut sync = args.clone();
                sync.endpoint = self.client.endpoint.clone();
                let report = crate::run(&sync).await?;
                self.message = format!(
                    "Claude Code: {} modelos, {}. Reinicia Claude Code.",
                    report.count,
                    if report.changed {
                        "actualizado"
                    } else {
                        "sin cambios"
                    }
                );
            }
            KeyCode::Char('e') if self.tab == 3 => {
                self.form = Some(Form {
                    title: "Trazas · contenido completo requiere consentimiento por 1 hora",
                    fields: vec![
                        Field::new(
                            "Habilitado: true / false",
                            self.settings["traces"]["enabled"]
                                .as_bool()
                                .unwrap_or(false),
                        ),
                        Field::new(
                            "Retención: 1 a 168 horas",
                            self.settings["traces"]["retention_hours"]
                                .as_u64()
                                .filter(|n| *n > 0)
                                .unwrap_or(72),
                        ),
                        Field::new(
                            "Consentimiento: CAPTURAR CONTENIDO (vacío: solo metadatos)",
                            "",
                        ),
                    ],
                    selected: 0,
                    action: Action::TracePolicy,
                })
            }
            KeyCode::Enter if self.tab == 3 => {
                if let Some(trace) = self.traces.get(self.selected) {
                    self.detail = Some(serde_json::to_string_pretty(trace)?);
                }
            }
            KeyCode::Char('d') if self.tab == 3 => {
                self.form = Some(Form {
                    title: "Borrar todas las trazas",
                    fields: vec![Field::new("Escribe ELIMINAR", "")],
                    selected: 0,
                    action: Action::ClearTraces,
                })
            }
            KeyCode::Char('n') if self.tab == 3 && self.traces.len() == 100 => {
                let id = text(self.traces.last().unwrap(), "id");
                self.trace_cursors.push(id);
                self.selected = 0;
                self.refresh().await?;
            }
            KeyCode::Char('p') if self.tab == 3 => {
                self.trace_cursors.pop();
                self.selected = 0;
                self.refresh().await?;
            }
            KeyCode::Char('x') if self.tab == 3 => {
                let directory = crate::control::config_path()?
                    .parent()
                    .unwrap()
                    .join("diagnostics");
                std::fs::create_dir_all(&directory)?;
                let path = directory.join(format!(
                    "traces-{}.json",
                    SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
                ));
                std::fs::write(&path, serde_json::to_vec_pretty(&self.traces)?)?;
                self.message = format!("Exportadas las 100 trazas visibles: {}", path.display());
            }
            _ => {}
        }
        Ok(false)
    }
    fn draw(&self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(4),
        ])
        .split(frame.area());
        let tabs = ["Proveedores", "Modelos", "Preferencias", "Trazas"]
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.tab {
                    format!("[ {t} ]")
                } else {
                    t.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("   ");
        frame.render_widget(
            Paragraph::new(tabs)
                .block(Block::bordered().title(format!("claude-proxy · {}", self.client.endpoint)))
                .style(Style::default().fg(Color::Cyan)),
            areas[0],
        );
        if let Some(form) = &self.form {
            let rows = form
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let value = if f.secret {
                        "•".repeat(f.value.chars().count())
                    } else {
                        f.value.clone()
                    };
                    ListItem::new(format!(
                        "{}\n  {}{}\n",
                        f.label,
                        value,
                        if i == form.selected { "▏" } else { "" }
                    ))
                    .style(if i == form.selected {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    })
                })
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(form.selected));
            frame.render_stateful_widget(
                List::new(rows).block(Block::bordered().title(form.title)),
                areas[1],
                &mut state,
            );
        } else if let Some(detail) = &self.detail {
            frame.render_widget(
                Paragraph::new(detail.as_str())
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(Block::bordered().title("Detalle · ↑↓ / PgUp PgDn · Esc cerrar")),
                areas[1],
            );
        } else if self.tab == 2 {
            let settings = &self.settings;
            let content = format!(
                "Modelo predeterminado: {}\nContexto: {}\nCompactación: {}\nTimeout: {} s\nURL Anthropic: {}\n\ne editar · R restaurar límites · s sincronizar Claude Code",
                text(settings, "default_model"),
                number(settings, "context_tokens"),
                number(settings, "compact_tokens"),
                number(settings, "timeout_seconds"),
                text(settings, "public_url")
            );
            frame.render_widget(
                Paragraph::new(content)
                    .block(Block::bordered().title("Preferencias del servidor"))
                    .wrap(Wrap { trim: false }),
                areas[1],
            );
        } else {
            let (title, rows): (String, Vec<String>) = match self.tab {
                0 => ("Conexiones · n nueva · e editar · Enter modelos · o OAuth · u desconectar · K borrar clave · d eliminar · m models.dev".into(),self.providers().iter().map(|p| format!("{}  {}  {}  {}",if p["enabled"] == true { "●" } else { "○" },text(p,"name"),text(p,"kind"),if p["subscription_connected"] == true { "sesión conectada" } else if p["has_api_key"] == true { "clave guardada" } else { "sin credencial" })).collect()),
                1 => (format!("{} · / filtrar: {} · espacio activar · f predeterminado · n manual · e editar · d retirar · Enter metadatos",self.providers().get(self.provider).map(|p| text(p,"name")).unwrap_or_default(),self.filter),self.model_rows().iter().map(|(m,_)| format!("{}  {}  {}",if m["enabled"] == true { "●" } else { "○" },text(m,"id"),text(m,"protocol"))).collect()),
                _ => (format!("Trazas · n/p páginas · e configurar · Enter detalle · x exportar · d borrar · captura {}",if self.settings["traces"]["enabled"] == true { "activa" } else { "desactivada" }),self.traces.iter().map(|t| format!("{}  {}  {}  eventos omitidos: {}",text(t,"id"),number(t,"created"),text(t,"outcome"),number(t,"dropped_events"))).collect()),
            };
            let rows = if rows.is_empty() {
                vec!["Sin elementos. Añade un proveedor con n o actualiza con r.".into()]
            } else {
                rows
            };
            let list = List::new(rows)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("› ");
            let mut state = ListState::default().with_selected(Some(self.selected));
            frame.render_stateful_widget(list, areas[1], &mut state);
        }
        let help = if self.form.is_some() {
            "Tab campos · Enter guardar · Esc cancelar · Ctrl+C salir"
        } else {
            "Tab / ←→ pestañas · ↑↓ navegar · r actualizar · s sincronizar Claude Code · q salir"
        };
        frame.render_widget(
            Paragraph::new(format!("{help}\n{}", self.message))
                .wrap(Wrap { trim: false })
                .block(Block::bordered()),
            areas[2],
        );
    }
}
fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").into()
}
fn number(v: &Value, k: &str) -> String {
    v[k].as_u64().map(|n| n.to_string()).unwrap_or_default()
}

pub async fn run(client: Client, args: &crate::Args) -> Result<()> {
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "El panel necesita un terminal. Para sincronizar usa --sync"
    );
    let mut panel = Panel::load(client).await?;
    let mut terminal = ratatui::init();
    let result = async {
        loop {
            panel.selected = panel.selected.min(panel.length().saturating_sub(1));
            terminal.draw(|frame| panel.draw(frame))?;
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
                    if let Some(form) = &mut panel.form {
                        form.fields[form.selected].value.clear();
                    }
                    continue;
                }
                match panel.action(key.code, args).await {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(e) => panel.message = format!("Error: {e:#}"),
                }
            }
        }
        anyhow::Ok(())
    }
    .await;
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_empty_panel_without_credentials() {
        let panel = Panel {
            client: Client::new("http://127.0.0.1:10801").unwrap(),
            settings: json!({"providers":[]}),
            catalog: vec![],
            traces: vec![],
            tab: 0,
            selected: 0,
            provider: 0,
            filter: String::new(),
            filtering: false,
            form: None,
            detail: None,
            scroll: 0,
            message: String::new(),
            configured: true,
            trace_cursors: vec![],
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 25)).unwrap();
        terminal.draw(|f| panel.draw(f)).unwrap();
        assert_eq!(panel.length(), 0);
    }
}
