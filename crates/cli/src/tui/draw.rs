use super::*;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

impl Panel {
    pub(super) fn draw(&self, frame: &mut Frame) {
        if frame.area().width < 80 || frame.area().height < 24 {
            frame.render_widget(
                Paragraph::new(
                    "Amplía el terminal a 80 × 24 para ver el panel.\nCtrl+C para salir.",
                )
                .wrap(Wrap { trim: false }),
                frame.area(),
            );
            return;
        }
        let [heading, body, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(4),
        ])
        .areas(frame.area());
        let [navigation, content] =
            Layout::horizontal([Constraint::Length(23), Constraint::Min(30)]).areas(body);
        let areas = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(content);
        frame.render_widget(
            Paragraph::new(format!(
                " claude-proxy  ·  Panel de control\n {}",
                self.client.endpoint
            ))
            .style(Style::default().fg(Color::Cyan)),
            heading,
        );
        let mut state = ListState::default().with_selected(Some(self.tab));
        frame.render_stateful_widget(
            List::new(["Proveedores", "Modelos", "Preferencias", "Trazas"])
                .block(panel_block(
                    if self.sidebar {
                        "Menú · ↑↓ Enter"
                    } else {
                        "Menú · Esc volver"
                    },
                    self.sidebar,
                ))
                .highlight_symbol("▸ ")
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            navigation,
            &mut state,
        );
        let guidance = if let Some(form) = &self.form {
            match form.action {
                Action::Setup => {
                    "Elige la base con ←→. Enter avanza; confirma en Configurar servidor."
                }
                Action::DeleteProvider(_) | Action::DeleteModel(_) | Action::ClearTraces => {
                    "Revisa la acción. Escribe ELIMINAR y confirma en Guardar cambios; Esc cancela."
                }
                Action::TracePolicy => {
                    "La captura usa metadatos. El contenido completo requiere tu consentimiento por 1 hora."
                }
                _ => {
                    "Enter avanza al siguiente campo. Las opciones se eligen con ←→. Al final, Guardar cambios."
                }
            }
        } else {
            match self.tab {
                0 => "Conecta tus proveedores y elige los modelos que usará Claude Code.",
                1 => {
                    "Elige un modelo y pulsa Enter para activarlo, editarlo o consultar sus datos."
                }
                2 => "Ajusta el proxy y sincroniza su catálogo con Claude Code.",
                _ => {
                    "Revisa cómo se transforman las peticiones y configura la captura de diagnóstico."
                }
            }
        };
        frame.render_widget(
            Paragraph::new(guidance).wrap(Wrap { trim: false }),
            areas[0],
        );
        if let Some(form) = &self.form {
            let mut rows = form
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let value = f.display(i == form.selected);
                    let label = if matches!(form.action, Action::Setup) && i == 1 {
                        if form.fields[0].value == "sqlite" {
                            "Ruta SQLite (vacío: directorio del servidor)"
                        } else {
                            "URL de conexión con usuario y contraseña (DSN)"
                        }
                    } else {
                        f.label
                    };
                    ListItem::new(format!("{label}\n  {value}\n")).style(if i == form.selected {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    })
                })
                .collect::<Vec<_>>();
            rows.push(ListItem::new(if matches!(form.action, Action::Setup) {
                "[ Configurar servidor ]"
            } else {
                "[ Guardar cambios ]"
            }));
            if self.configured {
                rows.push(ListItem::new("[ Cancelar ]"));
            }
            let mut state = ListState::default().with_selected(Some(form.selected));
            frame.render_stateful_widget(
                List::new(rows)
                    .block(panel_block(form.title, true))
                    .highlight_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("▸ "),
                areas[1],
                &mut state,
            );
        } else if let Some(detail) = &self.detail {
            frame.render_widget(
                Paragraph::new(detail.as_str())
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(panel_block("Detalle · ↑↓ / PgUp PgDn · Esc cerrar", true)),
                areas[1],
            );
        } else if self.tab == 2 {
            let settings = &self.settings;
            let content = format!(
                "Modelo predeterminado: {}\nContexto: {}\nCompactación: {}\nTimeout: {} s\nURL Anthropic: {}\n\nPulsa Enter para editar o sincronizar con Claude Code.",
                text(settings, "default_model"),
                number(settings, "context_tokens"),
                number(settings, "compact_tokens"),
                number(settings, "timeout_seconds"),
                text(settings, "public_url")
            );
            frame.render_widget(
                Paragraph::new(content)
                    .block(panel_block("Preferencias del servidor", !self.sidebar))
                    .wrap(Wrap { trim: false }),
                areas[1],
            );
        } else {
            let (title, rows): (String, Vec<String>) = match self.tab {
                0 => (
                    "Conexiones".into(),
                    self.providers()
                        .iter()
                        .map(|p| {
                            format!(
                                "{}  {} · {}\n   {}",
                                if p["enabled"] == true { "●" } else { "○" },
                                text(p, "name"),
                                choice_name(p["kind"].as_str().unwrap_or("")),
                                if p["zed_connected"] == true {
                                    "Cuenta Zed conectada"
                                } else if p["subscription_connected"] == true {
                                    "Sesión conectada"
                                } else if p["has_api_key"] == true {
                                    "Clave guardada"
                                } else {
                                    "Sin credencial · Enter para configurar"
                                }
                            )
                        })
                        .collect(),
                ),
                1 => (
                    format!(
                        "Modelos · {} · filtro: {}",
                        self.providers()
                            .get(self.provider)
                            .map(|p| text(p, "name"))
                            .unwrap_or_default(),
                        self.filter
                    ),
                    self.model_rows()
                        .iter()
                        .map(|(m, _)| {
                            format!(
                                "{}  {}\n   {} · {}",
                                if m["enabled"] == true { "●" } else { "○" },
                                text(m, "id"),
                                m["display_name"].as_str().unwrap_or(""),
                                choice_name(
                                    m["protocol"]
                                        .as_str()
                                        .or_else(|| m["suggested_protocol"].as_str())
                                        .unwrap_or("chat")
                                )
                            )
                        })
                        .collect(),
                ),
                _ => (
                    format!(
                        "Trazas · página {} · captura {}",
                        self.trace_cursors.len() + 1,
                        if self.settings["traces"]["enabled"] == true {
                            "activa"
                        } else {
                            "desactivada"
                        }
                    ),
                    self.traces
                        .iter()
                        .map(|t| {
                            format!(
                                "{}\n   {} · {} · eventos omitidos: {}",
                                text(t, "id"),
                                number(t, "created"),
                                text(t, "outcome"),
                                number(t, "dropped_events")
                            )
                        })
                        .collect(),
                ),
            };
            let rows = if rows.is_empty() {
                vec![match self.tab {
                    0 => "Todavía no hay proveedores.\nPulsa Enter y elige Añadir proveedor.",
                    1 => "No hay modelos en esta lista.\nPulsa Enter para añadir uno o revisar el catálogo.",
                    _ => "Todavía no hay trazas.\nPulsa Enter para configurar la captura.",
                }.into()]
            } else {
                rows
            };
            let list = List::new(rows)
                .block(panel_block(title, !self.sidebar))
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
            "↑↓ / Tab campos y botones · ←→ elegir / cursor · Enter continuar · Esc cancelar"
        } else if self.sidebar {
            "↑↓ elegir sección · Enter abrir · F1 ayuda · q salir"
        } else if self.menu.is_some() {
            "↑↓ elegir acción · Enter aceptar · Esc volver"
        } else if self.filtering {
            "Escribe para filtrar · Enter aceptar · Esc volver"
        } else if self.detail.is_some() {
            "↑↓ desplazar · PgUp/PgDn página · Esc cerrar"
        } else {
            "↑↓ elegir · Enter acciones · Esc menú principal · F1 ayuda"
        };
        frame.render_widget(
            Paragraph::new(format!("{help}\n{}", self.message))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(if self.message.starts_with("Error:") {
                    Color::Red
                } else {
                    Color::Gray
                }))
                .block(panel_block("Ayuda", false)),
            footer,
        );
        frame.render_widget(
            Paragraph::new(if self.filtering {
                format!("Buscar: {}▏", self.filter)
            } else {
                "● activo   ○ desactivado · Enter muestra las acciones disponibles".into()
            })
            .style(Style::default().fg(Color::DarkGray)),
            areas[2],
        );
        if let Some(menu) = &self.menu {
            let area = centered(frame.area(), 62, menu.items.len() as u16 + 2);
            frame.render_widget(Clear, area);
            let mut state = ListState::default().with_selected(Some(menu.selected));
            frame.render_stateful_widget(
                List::new(
                    menu.items
                        .iter()
                        .map(|(label, _)| *label)
                        .collect::<Vec<_>>(),
                )
                .block(panel_block("¿Qué quieres hacer?", true))
                .highlight_symbol("▸ ")
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                area,
                &mut state,
            );
        }
        if self.help {
            let area = centered(frame.area(), 72, 15);
            frame.render_widget(Clear, area);
            frame.render_widget(Paragraph::new("↑ ↓            Elegir sección, elemento o acción\nEnter          Abrir, continuar o aceptar el botón seleccionado\nEsc            Volver o cancelar el formulario\nTab / Mayús+Tab Cambiar de campo o volver al menú\n← →            Elegir una opción o mover el cursor de texto\nInicio / Fin   Principio / final de la lista o campo\nRePág / AvPág  Desplazar listas y detalles\nCtrl+U         Vaciar el campo de texto\nF1 / ?         Mostrar esta ayuda\nCtrl+C         Salir\n\nLas acciones habituales están en Enter.\nEsc, Enter o F1 cierran la ayuda.")
                .block(panel_block("Ayuda · claude-proxy", true)), area);
        }
    }
}

fn panel_block<'a>(title: impl Into<ratatui::text::Line<'a>>, focused: bool) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(title)
}
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
