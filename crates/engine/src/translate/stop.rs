use anyhow::{Result, ensure};
use serde_json::Value;

// Streaming stop matching retains only a possible delimiter prefix. It works
// across arbitrary UTF-8 chunks without exposing part of a stop sequence.
#[derive(Default)]
pub(crate) struct StopFilter {
    sequences: Vec<String>,
    pending: String,
    pub matched: Option<String>,
}

impl StopFilter {
    pub fn new(req: &Value) -> Result<Self> {
        let Some(value) = req.get("stop_sequences") else {
            return Ok(Self::default());
        };
        let all = value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("stop_sequences debe ser una lista"))?;
        ensure!(all.len() <= 128, "Demasiadas secuencias de parada");
        let mut sequences = Vec::new();
        for value in all {
            let value = value
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 4096)
                .ok_or_else(|| anyhow::anyhow!("Secuencia de parada vacía o demasiado grande"))?;
            sequences.push(value.into());
        }
        Ok(Self {
            sequences,
            ..Self::default()
        })
    }

    pub fn push(&mut self, text: &str) -> String {
        if self.matched.is_some() {
            return String::new();
        }
        if self.sequences.is_empty() {
            return text.into();
        }
        self.pending.push_str(text);
        if let Some((position, sequence)) = self
            .sequences
            .iter()
            .filter_map(|s| self.pending.find(s).map(|p| (p, s)))
            .min_by_key(|(p, _)| *p)
        {
            let output = self.pending[..position].to_string();
            self.matched = Some(sequence.clone());
            self.pending.clear();
            return output;
        }
        let split = self
            .pending
            .char_indices()
            .find_map(|(position, _)| {
                self.sequences
                    .iter()
                    .any(|s| s.starts_with(&self.pending[position..]))
                    .then_some(position)
            })
            .unwrap_or(self.pending.len());
        let output = self.pending[..split].to_string();
        self.pending.drain(..split);
        output
    }

    pub fn flush(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}
