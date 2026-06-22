//! Redact likely secrets from tool output before context injection and logging.

use regex::Regex;
use std::sync::LazyLock;

static SK_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"sk-[a-zA-Z0-9_-]{8,}").expect("sk regex"));

static ASSIGN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(api[_-]?key|password|secret|token)\s*[=:]\s*\S+").expect("assign regex")
});

/// Scrub common credential patterns from tool output.
pub fn tool_output(s: &str) -> String {
    let after_sk = SK_PATTERN.replace_all(s, "sk-[REDACTED]");
    ASSIGN_PATTERN
        .replace_all(&after_sk, "$1=[REDACTED]")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_env_style_secret() {
        let raw = "API_KEY=sk-super-secret-12345\nDB_HOST=localhost\n";
        let out = tool_output(raw);
        assert!(!out.contains("sk-super-secret"));
        assert!(out.contains("API_KEY=[REDACTED]"));
    }

    #[test]
    fn redacts_bare_sk_token() {
        let raw = "token: sk-live-abcdefghijklmnop";
        let out = tool_output(raw);
        assert!(!out.contains("sk-live"));
        assert!(out.contains("[REDACTED]"));
    }
}