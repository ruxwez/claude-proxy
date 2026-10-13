mod draw;
mod field;
use field::{Field, choice_name};

use crate::control::Client;
use anyhow::{Context, Result, ensure};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};
use std::{
    io::IsTerminal,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Menu {
    items: Vec<(&'static str, KeyCode)>,
    selected: usize,
}
enum Action {
    Provider(Option<usize>),
    Model(Option<usize>),
    Preferences,
    Setup,
    TracePolicy,
    ZedAccount(usize),
    DeleteProvider(usize),
    DeleteModel(usize),
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
    sidebar: bool,
    menu: Option<Menu>,
    help: bool,
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
            sidebar: true,
            menu: None,
            help: false,
        };
        if !configured {
            panel.form = Some(Form {
                title: "Configurar servidor",
                fields: vec![
                    Field::choice("Base de datos", "sqlite", &["sqlite", "postgres", "mysql"]),
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
            Field::choice(
                "Tipo de proveedor",
                text(&p, "kind"),
                &[
                    "opencode_go",
                    "openai",
                    "chatgpt",
                    "google",
                    "openai_compatible",
                    "zed",
                ],
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
            Field::choice(
                "Habilitado",
                p["enabled"].as_bool().unwrap_or(true),
                &["true", "false"],
            ),
        ];
        fields[4].secret = true;
        if p["kind"] == "zed" {
            fields[4] = Field::choice(
                "Cuenta Zed: conectar desde el menú del proveedor",
                "",
                &[""],
            );
        }
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
                Field::choice(
                    "Protocolo",
                    model["protocol"]
                        .as_str()
                        .or_else(|| model["suggested_protocol"].as_str())
                        .unwrap_or(
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
                    &["chat", "responses", "messages"],
                ),
                Field::choice(
                    "Habilitado",
                    model["enabled"].as_bool().unwrap_or(true),
                    &["true", "false"],
                ),
            ],
            selected: 0,
            action: Action::Model(index),
        });
        if self.current_provider().is_ok_and(|p| p["kind"] == "zed") {
            let original = model["zed_provider"].as_str().unwrap_or("anthropic");
            let form = self.form.as_mut().unwrap();
            let protocol = if matches!(original, "open_ai" | "x_ai") {
                "responses"
            } else {
                "messages"
            };
            form.fields[2] = Field::choice(
                "Protocolo interno (automático; salida siempre Anthropic)",
                protocol,
                &[protocol],
            );
            form.fields.push(Field::choice(
                "Proveedor original del modelo Zed",
                original,
                &["anthropic", "open_ai", "google", "x_ai"],
            ));
        }
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
                        "openai_compatible",
                        "zed"
                    ]
                    .contains(&kind),
                    "Tipo de proveedor inválido"
                );
                let base = if values[3].is_empty() {
                    match kind {
                        "opencode_go" => "https://opencode.ai/zen/go/v1",
                        "openai" | "chatgpt" => "https://api.openai.com/v1",
                        "google" => "https://generativelanguage.googleapis.com/v1beta/openai",
                        "zed" => "https://cloud.zed.dev",
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
                let mut model = json!({"id":values[0],"display_name":values[1],"protocol":values[2],"enabled":boolean(3)?,"credential":credential});
                if self.current_provider()?["kind"] == "zed" {
                    model["zed_provider"] = json!(values[4]);
                    model["protocol"] =
                        json!(if matches!(values[4].as_str(), "open_ai" | "x_ai") {
                            "responses"
                        } else {
                            "messages"
                        });
                }
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
            Action::ZedAccount(index) => {
                let id = text(&self.providers()[index], "id");
                let body = json!({"user_id":values[0].parse::<u32>()?,"access_token":values[1],"organization_id":values[2],"client_version":values[3]});
                self.client
                    .request(
                        reqwest::Method::POST,
                        &format!("/api/providers/{id}/zed/credentials"),
                        Some(&body),
                    )
                    .await?;
                self.form = None;
                self.refresh().await?;
                self.message = "Cuenta Zed conectada".into();
                return Ok(());
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
            Action::DeleteModel(index) => {
                ensure!(values[0] == "ELIMINAR", "Escribe ELIMINAR");
                next["providers"][self.provider]["models"]
                    .as_array_mut()
                    .unwrap()
                    .remove(index);
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
    fn open_menu(&mut self) {
        use KeyCode::{Char, Enter};
        let mut items = match self.tab {
            0 => {
                let mut items = vec![];
                if self.selected < self.providers().len() {
                    items.extend([
                        ("Ver y elegir modelos", Enter),
                        ("Editar conexión", Char('e')),
                        ("Activar / desactivar proveedor", Char(' ')),
                    ]);
                    if self.providers()[self.selected]["kind"] == "chatgpt" {
                        items.push(("Conectar ChatGPT en el navegador", Char('o')));
                        items.push(("Desconectar sesión ChatGPT", Char('u')));
                    }
                    if self.providers()[self.selected]["kind"] == "zed" {
                        items.push(("Conectar Zed en el navegador", Char('o')));
                        items.push(("Introducir credenciales de Zed", Char('z')));
                        items.push(("Desconectar cuenta Zed", Char('u')));
                        if self.providers()[self.selected]["zed_connected"] != true {
                            let connect =
                                items.iter().position(|(_, key)| *key == Char('o')).unwrap();
                            items.swap(0, connect);
                        }
                    } else {
                        items.push(("Borrar API key guardada", Char('K')));
                    }
                    items.push(("Eliminar proveedor…", Char('d')));
                }
                items.push(("Añadir proveedor", Char('n')));
                items.push(("Consultar proveedores de models.dev", Char('m')));
                items
            }
            1 => {
                let mut items = vec![];
                if let Some((_, index)) = self.model_rows().get(self.selected) {
                    items.extend([
                        ("Activar / desactivar modelo", Char(' ')),
                        ("Editar nombre y protocolo", Char('e')),
                        ("Consultar metadatos", Enter),
                    ]);
                    if index.is_some() {
                        items.push(("Usar como modelo predeterminado", Char('f')));
                        items.push(("Retirar modelo…", Char('d')));
                    }
                }
                items.extend([
                    ("Añadir modelo manual", Char('n')),
                    ("Buscar modelo", Char('/')),
                ]);
                items
            }
            2 => vec![
                ("Editar preferencias", Char('e')),
                ("Restaurar límites de contexto", Char('R')),
            ],
            _ => {
                let mut items = vec![("Configurar captura de trazas", Char('e'))];
                if !self.traces.is_empty() {
                    items.push(("Inspeccionar traza", Enter));
                    items.push(("Exportar página de trazas", Char('x')));
                }
                if self.traces.len() == 100 {
                    items.push(("Página siguiente", Char('n')));
                }
                if !self.trace_cursors.is_empty() {
                    items.push(("Página anterior", Char('p')));
                }
                items.push(("Borrar todas las trazas…", Char('d')));
                items
            }
        };
        items.extend([
            ("Actualizar datos", Char('r')),
            ("Sincronizar modelos con Claude Code", Char('s')),
            ("Salir", Char('q')),
        ]);
        self.menu = Some(Menu { items, selected: 0 });
    }
    async fn enter_section(&mut self) -> Result<()> {
        self.sidebar = false;
        self.selected = 0;
        if self.tab == 1 && !self.providers().is_empty() {
            self.discover(false).await?;
        } else if self.tab == 3 {
            self.refresh().await?;
        }
        Ok(())
    }
    fn operation(&self, key: KeyCode) -> Option<&'static str> {
        if self.help || self.detail.is_some() || self.filtering {
            return None;
        }
        if let Some(form) = &self.form {
            return (key == KeyCode::Enter && form.selected == form.fields.len())
                .then_some("Guardando en el servidor…");
        }
        if self.sidebar {
            return (matches!(key, KeyCode::Enter | KeyCode::Right | KeyCode::Tab)
                && matches!(self.tab, 1 | 3))
            .then_some("Consultando datos del servidor…");
        }
        let key = if let Some(menu) = &self.menu {
            if key != KeyCode::Enter {
                return None;
            }
            menu.items[menu.selected].1
        } else {
            if key == KeyCode::Enter {
                return None;
            }
            key
        };
        match key {
            KeyCode::Enter if self.tab == 0 => Some("Consultando catálogo de modelos…"),
            KeyCode::Char('s') => Some("Sincronizando modelos con Claude Code…"),
            KeyCode::Char('r' | 'm' | 'o' | 'u' | 'K' | ' ' | 'f' | 'R' | 'x') => {
                Some("Procesando la acción seleccionada…")
            }
            KeyCode::Char('n' | 'p') if self.tab == 3 => Some("Consultando página de trazas…"),
            _ => None,
        }
    }
    async fn action(&mut self, mut key: KeyCode, args: &crate::Args) -> Result<bool> {
        if self.help {
            if matches!(
                key,
                KeyCode::Esc | KeyCode::Enter | KeyCode::F(1) | KeyCode::Char('?')
            ) {
                self.help = false;
            }
            return Ok(false);
        }
        if key == KeyCode::F(1)
            || (key == KeyCode::Char('?') && self.form.is_none() && !self.filtering)
        {
            self.help = true;
            return Ok(false);
        }
        if let Some(form) = &mut self.form {
            let buttons = if self.configured { 2 } else { 1 };
            let count = form.fields.len() + buttons;
            match key {
                KeyCode::Esc if self.configured => self.form = None,
                KeyCode::Tab | KeyCode::Down => form.selected = (form.selected + 1) % count,
                KeyCode::BackTab | KeyCode::Up => {
                    form.selected = (form.selected + count - 1) % count
                }
                KeyCode::Enter if form.selected < form.fields.len() => {
                    form.selected += 1;
                }
                KeyCode::Enter if form.selected == form.fields.len() => self.submit().await?,
                KeyCode::Enter => self.form = None,
                _ => {
                    if let Some(field) = form.fields.get_mut(form.selected) {
                        field.edit(key);
                    }
                    if matches!(form.action, Action::Model(_)) && form.fields.len() == 5 {
                        let protocol =
                            if matches!(form.fields[4].value.as_str(), "open_ai" | "x_ai") {
                                "responses"
                            } else {
                                "messages"
                            };
                        form.fields[2].value = protocol.into();
                        form.fields[2].choices = vec![protocol];
                    }
                    if matches!(form.action, Action::Provider(_)) {
                        if form.fields[2].value == "zed" {
                            form.fields[4] = Field::choice(
                                "Cuenta Zed: conectar desde el menú del proveedor",
                                "",
                                &[""],
                            );
                        } else {
                            form.fields[4].label = "API key (vacío: conservar)";
                            form.fields[4].choices.clear();
                            form.fields[4].secret = true;
                        }
                    }
                }
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
        if let Some(menu) = &mut self.menu {
            match key {
                KeyCode::Esc | KeyCode::Left => {
                    self.menu = None;
                    return Ok(false);
                }
                KeyCode::Down => {
                    menu.selected = (menu.selected + 1) % menu.items.len();
                    return Ok(false);
                }
                KeyCode::Up => {
                    menu.selected = (menu.selected + menu.items.len() - 1) % menu.items.len();
                    return Ok(false);
                }
                KeyCode::Home => {
                    menu.selected = 0;
                    return Ok(false);
                }
                KeyCode::End => {
                    menu.selected = menu.items.len() - 1;
                    return Ok(false);
                }
                KeyCode::Enter => {
                    key = menu.items[menu.selected].1;
                    self.menu = None;
                }
                _ => return Ok(false),
            }
        } else {
            if self.sidebar {
                match key {
                    KeyCode::Down => {
                        self.tab = (self.tab + 1) % 4;
                        self.selected = 0;
                    }
                    KeyCode::Up => {
                        self.tab = (self.tab + 3) % 4;
                        self.selected = 0;
                    }
                    KeyCode::Enter | KeyCode::Right | KeyCode::Tab => self.enter_section().await?,
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(true),
                    _ => {}
                }
                return Ok(false);
            }
            match key {
                KeyCode::Enter | KeyCode::Right | KeyCode::F(2) => {
                    self.open_menu();
                    return Ok(false);
                }
                KeyCode::Esc | KeyCode::Left | KeyCode::BackTab | KeyCode::Tab => {
                    self.sidebar = true;
                    return Ok(false);
                }
                _ => {}
            }
        }
        match key {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.length().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.length().saturating_sub(1),
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.length().saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
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
                self.filter.clear();
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
                    self.form = Some(Form {
                        title: "Retirar modelo · se quitará de Claude Code al sincronizar",
                        fields: vec![Field::new("Escribe ELIMINAR para confirmar", "")],
                        selected: 0,
                        action: Action::DeleteModel(index),
                    });
                }
            }
            KeyCode::Char('o') if self.tab == 0 && self.selected < self.providers().len() => {
                let id = text(&self.providers()[self.selected], "id");
                let resource = if self.providers()[self.selected]["kind"] == "zed" {
                    "zed/auth"
                } else {
                    "oauth"
                };
                let result = self
                    .client
                    .request(
                        reqwest::Method::POST,
                        &format!("/api/providers/{id}/{resource}"),
                        Some(&json!({})),
                    )
                    .await?;
                let url = result["url"].as_str().context("Falta URL OAuth")?;
                let _ = webbrowser::open(url);
                self.detail = Some(format!(
                    "Completa la conexión en el navegador. Debe poder acceder al puerto REST del servidor en 127.0.0.1.\n\nVuelve a Proveedores y elige Actualizar datos.\n\n{url}"
                ));
            }
            KeyCode::Char('z')
                if self.tab == 0
                    && self.selected < self.providers().len()
                    && self.providers()[self.selected]["kind"] == "zed" =>
            {
                let mut token = Field::new("Token de cuenta Zed (no token LLM temporal)", "");
                token.secret = true;
                self.form = Some(Form {
                    title: "Conectar cuenta Zed · credenciales guardadas en el servidor",
                    fields: vec![
                        Field::new("ID numérico de usuario Zed", ""),
                        token,
                        Field::new("Organización (vacío: predeterminada de la cuenta)", ""),
                        Field::new(
                            "Versión de cliente (vacío: predeterminada del adaptador)",
                            "",
                        ),
                    ],
                    selected: 0,
                    action: Action::ZedAccount(self.selected),
                });
            }
            KeyCode::Char('u') | KeyCode::Char('K')
                if self.tab == 0 && self.selected < self.providers().len() =>
            {
                let id = text(&self.providers()[self.selected], "id");
                let resource = if self.providers()[self.selected]["kind"] == "zed" {
                    "zed/credentials"
                } else if key == KeyCode::Char('K') {
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
                        Field::choice(
                            "Capturar trazas",
                            self.settings["traces"]["enabled"]
                                .as_bool()
                                .unwrap_or(false),
                            &["false", "true"],
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
                self.message = format!(
                    "Exportadas {} trazas: {}",
                    self.traces.len(),
                    path.display()
                );
            }
            _ => {}
        }
        Ok(false)
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
                let size = terminal.size()?;
                if size.width < 80 || size.height < 24 {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
                    if !panel.help
                        && let Some(form) = &mut panel.form
                        && let Some(field) = form.fields.get_mut(form.selected)
                        && field.choices.is_empty()
                    {
                        field.value.clear();
                        field.cursor = 0;
                    }
                    continue;
                }
                if let Some(message) = panel.operation(key.code) {
                    panel.message = message.into();
                    terminal.draw(|frame| panel.draw(frame))?;
                }
                match panel.action(key.code, args).await {
                    Ok(true) => break,
                    Ok(false) => {
                        if panel.message.ends_with('…') {
                            panel.message = "Listo".into();
                        }
                    }
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
    use clap::Parser;

    fn panel() -> Panel {
        Panel {
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
            sidebar: true,
            menu: None,
            help: false,
        }
    }
    #[test]
    fn renders_empty_panel_without_credentials() {
        let panel = panel();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 25)).unwrap();
        terminal.draw(|f| panel.draw(f)).unwrap();
        assert_eq!(panel.length(), 0);
    }

    #[tokio::test]
    async fn arrows_and_enter_open_actions_and_advance_fields_without_saving() {
        let mut panel = panel();
        let args = crate::Launch::try_parse_from(["claude-proxy"])
            .unwrap()
            .args;
        panel.action(KeyCode::Enter, &args).await.unwrap();
        assert!(!panel.sidebar);
        panel.action(KeyCode::Enter, &args).await.unwrap();
        assert_eq!(panel.menu.as_ref().unwrap().items[0].0, "Añadir proveedor");
        panel.action(KeyCode::Enter, &args).await.unwrap();
        assert!(panel.form.is_some());
        for key in [KeyCode::Char('a'), KeyCode::Enter, KeyCode::Enter] {
            panel.action(key, &args).await.unwrap();
        }
        let form = panel.form.as_ref().unwrap();
        assert_eq!(form.selected, 2);
        assert_eq!(form.fields[0].value, "a");
        assert!(panel.providers().is_empty());
        panel.action(KeyCode::Left, &args).await.unwrap();
        assert_eq!(panel.form.as_ref().unwrap().fields[2].value, "google");
        panel.form.as_mut().unwrap().selected = 8; // Cancelar, after seven fields + Guardar.
        panel.action(KeyCode::Enter, &args).await.unwrap();
        assert!(panel.form.is_none());
        panel.action(KeyCode::Esc, &args).await.unwrap();
        assert!(panel.sidebar);
        panel.action(KeyCode::Down, &args).await.unwrap();
        assert_eq!(panel.tab, 1);
    }

    #[test]
    fn text_cursor_handles_unicode_delete_and_secret_values() {
        let mut field = Field::new("Clave", "á🔑z");
        field.secret = true;
        field.edit(KeyCode::Left);
        field.edit(KeyCode::Backspace);
        field.edit(KeyCode::Char('ñ'));
        assert_eq!(field.value, "áñz");
        field.edit(KeyCode::Home);
        field.edit(KeyCode::Delete);
        assert_eq!(field.value, "ñz");
        assert_eq!(field.display(true), "▏••");
        let mut choice = Field::choice("Habilitado", true, &["true", "false"]);
        choice.edit(KeyCode::Char('x'));
        assert_eq!(choice.value, "true");
        choice.edit(KeyCode::Right);
        assert_eq!(choice.value, "false");
        assert!(choice.display(true).contains("No"));
    }

    #[tokio::test]
    async fn help_and_action_menu_cancel_preserve_selection() {
        let mut panel = panel();
        let args = crate::Launch::try_parse_from(["claude-proxy"])
            .unwrap()
            .args;
        panel.sidebar = false;
        panel.action(KeyCode::Enter, &args).await.unwrap();
        panel.action(KeyCode::End, &args).await.unwrap();
        panel.action(KeyCode::F(1), &args).await.unwrap();
        assert!(panel.help);
        panel.action(KeyCode::Esc, &args).await.unwrap();
        assert!(!panel.help);
        assert!(panel.menu.is_some());
        panel.action(KeyCode::Esc, &args).await.unwrap();
        assert!(panel.menu.is_none());
        assert!(!panel.sidebar);
    }
}
