use crossterm::event::KeyCode;

pub(super) struct Field {
    pub(super) label: &'static str,
    pub(super) value: String,
    pub(super) secret: bool,
    pub(super) choices: Vec<&'static str>,
    pub(super) cursor: usize,
}
impl Field {
    pub(super) fn new(label: &'static str, value: impl ToString) -> Self {
        let value = value.to_string();
        Self {
            label,
            cursor: value.chars().count(),
            value,
            secret: false,
            choices: vec![],
        }
    }
    pub(super) fn choice(
        label: &'static str,
        value: impl ToString,
        choices: &[&'static str],
    ) -> Self {
        let mut field = Self::new(label, value);
        field.choices = choices.to_vec();
        field
    }
    pub(super) fn edit(&mut self, key: KeyCode) {
        if !self.choices.is_empty() {
            let index = self
                .choices
                .iter()
                .position(|v| *v == self.value)
                .unwrap_or(0);
            let next = match key {
                KeyCode::Left => (index + self.choices.len() - 1) % self.choices.len(),
                KeyCode::Right | KeyCode::Char(' ') => (index + 1) % self.choices.len(),
                _ => return,
            };
            self.value = self.choices[next].into();
            return;
        }
        let byte = |position| {
            self.value
                .char_indices()
                .nth(position)
                .map_or(self.value.len(), |(i, _)| i)
        };
        match key {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.value.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.chars().count(),
            KeyCode::Char(c) if !c.is_control() => {
                self.value.insert(byte(self.cursor), c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.value
                    .replace_range(byte(self.cursor - 1)..byte(self.cursor), "");
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < self.value.chars().count() => {
                self.value
                    .replace_range(byte(self.cursor)..byte(self.cursor + 1), "");
            }
            _ => {}
        }
    }
    pub(super) fn display(&self, focused: bool) -> String {
        if !self.choices.is_empty() {
            return format!("◀  {}  ▶", choice_name(&self.value));
        }
        let chars: Vec<_> = self
            .value
            .chars()
            .map(|c| if self.secret { '•' } else { c })
            .collect();
        let start = if focused {
            self.cursor.saturating_sub(35)
        } else {
            0
        };
        let mut result = String::new();
        if start > 0 {
            result.push('…');
        }
        for index in start..=chars.len().min(start + 60) {
            if focused && index == self.cursor {
                result.push('▏');
            }
            if let Some(c) = chars.get(index) {
                result.push(*c);
            }
        }
        result
    }
}

pub(super) fn choice_name(value: &str) -> &str {
    match value {
        "true" => "Sí",
        "false" => "No",
        "sqlite" => "SQLite · archivo local",
        "postgres" => "PostgreSQL",
        "mysql" => "MySQL",
        "opencode_go" => "OpenCode Go",
        "openai" => "OpenAI",
        "chatgpt" => "ChatGPT · suscripción",
        "google" => "Google",
        "openai_compatible" => "API compatible con OpenAI",
        "chat" => "Chat Completions",
        "responses" => "Responses",
        "messages" => "Anthropic Messages",
        "zed" => "Zed · modelos alojados",
        "anthropic" => "Anthropic · Claude",
        "open_ai" => "OpenAI · GPT",
        "x_ai" => "xAI · Grok",
        _ => value,
    }
}
