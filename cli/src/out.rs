//! Machine-readable output contract. `--json` (or `L8B_JSON=1`) prints a
//! single JSON object on stdout; progress is suppressed; secrets are never
//! serialized. Errors print `{"ok":false,"error":{"message","hint"}}` and
//! exit 1.

use serde::Serialize;

#[derive(Clone, Copy, Default)]
pub struct Out {
    pub json: bool,
}

/// Marker separating an error message from its recovery hint.
const HINT: &str = "\nHINT: ";

impl Out {
    /// Resolve from the `--json` flag or `L8B_JSON=1`.
    pub fn from_flag(json_flag: bool) -> Self {
        let env = std::env::var("L8B_JSON").is_ok_and(|v| v == "1" || v == "true");
        Self { json: json_flag || env }
    }

    /// Human-only progress line.
    pub fn note(&self, msg: &str) {
        if !self.json {
            println!("{msg}");
        }
    }

    /// Print `{"ok": true, ...payload}` in JSON mode.
    pub fn ok<T: Serialize>(&self, payload: &T) {
        if let Some(value) = self.ok_value(payload) {
            println!("{value}");
        }
    }

    fn ok_value<T: Serialize>(&self, payload: &T) -> Option<String> {
        if !self.json {
            return None;
        }
        let map = match serde_json::to_value(payload) {
            Ok(serde_json::Value::Object(mut map)) => {
                map.insert("ok".to_string(), serde_json::Value::Bool(true));
                map
            }
            _ => serde_json::Map::new(),
        };
        serde_json::to_string(&map).ok()
    }
}

/// Build an error that carries a recovery hint.
pub fn fail(message: impl Into<String>, hint: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!("{}{}{}", message.into(), HINT, hint.into())
}

/// Split a formatted error chain into (message, hint).
pub fn split_hint(err: &anyhow::Error) -> (String, Option<String>) {
    let full = format!("{err:#}");
    match full.split_once(HINT) {
        Some((message, hint)) => (message.to_string(), Some(hint.to_string())),
        None => (full, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    struct Payload {
        project_id: String,
        url: Option<String>,
    }

    #[test]
    fn ok_value_flattens_payload_with_ok_true() {
        let out = Out { json: true };
        let value =
            out.ok_value(&Payload { project_id: "web".into(), url: Some("https://web.l8b.in".into()) }).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&value).unwrap();
        assert_eq!(parsed, json!({"ok": true, "project_id": "web", "url": "https://web.l8b.in"}));
    }

    #[test]
    fn ok_value_is_none_in_human_mode() {
        let out = Out { json: false };
        assert!(out.ok_value(&Payload { project_id: "web".into(), url: None }).is_none());
    }

    #[test]
    fn fail_carries_hint_through_chain() {
        let err = fail("not authenticated", "run: l8b login --server <url> --pair");
        let (message, hint) = split_hint(&err);
        assert_eq!(message, "not authenticated");
        assert_eq!(hint.as_deref(), Some("run: l8b login --server <url> --pair"));
    }

    #[test]
    fn split_hint_without_marker() {
        let err = anyhow::anyhow!("plain failure");
        let (message, hint) = split_hint(&err);
        assert_eq!(message, "plain failure");
        assert!(hint.is_none());
    }
}
