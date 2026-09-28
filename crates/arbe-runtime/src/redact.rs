//! Keeps known secret values out of what the harness stores and shows.
//!
//! A tool can easily print a secret — `env`, `cat .env`, an error message
//! echoing a token — and without this the value would be sent to the
//! model, written to `turns.jsonl`, and shown in the transcript. The
//! redactor replaces every occurrence of a known secret with
//! `[REDACTED]` before any of that happens.

use serde_json::Value;

pub const REDACTED: &str = "[REDACTED]";

/// Values shorter than this aren't treated as secrets: too likely to occur
/// in ordinary output by chance (and too short to be a real credential).
const MIN_SECRET_LEN: usize = 8;

/// Environment variable name endings that mark the value as a secret.
const SECRET_NAME_SUFFIXES: &[&str] = &[
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIALS",
];

#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Longest first, so a secret containing another is replaced whole.
    secrets: Vec<String>,
}

impl Redactor {
    /// Redacts `explicit` values plus every environment variable whose name
    /// ends in `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, ... (case-insensitive).
    pub fn new(
        explicit: impl IntoIterator<Item = String>,
        env: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let from_env = env.into_iter().filter_map(|(name, value)| {
            let upper = name.to_ascii_uppercase();
            SECRET_NAME_SUFFIXES
                .iter()
                .any(|suffix| upper.ends_with(suffix))
                .then_some(value)
        });
        let mut secrets: Vec<String> = explicit
            .into_iter()
            .chain(from_env)
            .map(|s| s.trim().to_string())
            .filter(|s| s.chars().count() >= MIN_SECRET_LEN)
            .collect();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Self { secrets }
    }

    /// From the process environment.
    pub fn from_process_env(explicit: impl IntoIterator<Item = String>) -> Self {
        Self::new(explicit, std::env::vars())
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REDACTED);
            }
        }
        out
    }

    /// Redacts every string inside a JSON value.
    pub fn redact_value(&self, value: Value) -> Value {
        if self.secrets.is_empty() {
            return value;
        }
        match value {
            Value::String(s) => Value::String(self.redact(&s)),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| self.redact_value(v)).collect())
            }
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, self.redact_value(v)))
                    .collect(),
            ),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn redacts_explicit_secrets_and_secret_looking_env_vars() {
        let r = Redactor::new(
            ["sk-explicit-123456".to_string()],
            env(&[
                ("GITHUB_TOKEN", "ghp_abcdef123456"),
                ("my_api_key", "lowercase-name-too"),
                ("PATH", "/usr/bin:/bin:/usr/local/bin"),
                ("SHORT_TOKEN", "abc"),
            ]),
        );
        let text = "key=sk-explicit-123456 gh=ghp_abcdef123456 x=lowercase-name-too path=/usr/bin:/bin:/usr/local/bin t=abc";
        assert_eq!(
            r.redact(text),
            "key=[REDACTED] gh=[REDACTED] x=[REDACTED] path=/usr/bin:/bin:/usr/local/bin t=abc"
        );
    }

    #[test]
    fn a_longer_secret_containing_a_shorter_one_is_replaced_whole() {
        let r = Redactor::new(
            ["abcdefgh".to_string(), "abcdefgh-longer".to_string()],
            vec![],
        );
        assert_eq!(r.redact("abcdefgh-longer"), "[REDACTED]");
    }

    #[test]
    fn redacts_inside_json_values() {
        let r = Redactor::new(["supersecretvalue".to_string()], vec![]);
        let v = r.redact_value(
            json!({"stdout": "token supersecretvalue", "n": 1, "list": ["supersecretvalue"]}),
        );
        assert_eq!(
            v,
            json!({"stdout": "token [REDACTED]", "n": 1, "list": ["[REDACTED]"]})
        );
    }

    #[test]
    fn nothing_to_redact_leaves_text_alone() {
        assert_eq!(Redactor::default().redact("anything"), "anything");
    }
}
