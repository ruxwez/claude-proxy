//! Pure protocol conversion. No HTTP, database, filesystem or async runtime.
pub mod stream;
pub mod translate;
pub mod zed;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Chat,
    Messages,
    Responses,
}
pub mod diagnostics;
#[cfg(test)]
mod protocol_tests;

pub fn protocol_for_model(model: &str) -> Protocol {
    let id = model.to_lowercase();
    if id.contains("muse-spark")
        || ["grok-4.6", "grok-4.7", "gpt-6-luna", "gpt-5.6-luna"]
            .iter()
            .any(|p| id.starts_with(p))
    {
        Protocol::Responses
    } else if [
        "claude-haiku-5-5",
        "minimax-m",
        "qwen3.7-plus",
        "qwen3.8-max",
        "qwen3.8-flash",
    ]
    .iter()
    .any(|p| id.starts_with(p))
    {
        Protocol::Messages
    } else {
        Protocol::Chat
    }
}
