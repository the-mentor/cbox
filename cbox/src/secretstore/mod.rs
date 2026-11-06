//! Pure data rules for stored secrets: names, hosts, env vars, JSON form, size limit.

// KeyringStore is compiled everywhere, but only usable where a store is registered (macOS today).
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use anyhow::{Result, bail};

mod index;
mod service;
mod store;
pub use index::Index;
pub use service::{EditChanges, EditOutcome, HostChange, ListRow, RowState, check_absent, create, delete, edit, exists, list};
pub use store::{SecretStore, open_default};
use serde::{Deserialize, Serialize};

/// Windows Credential Manager (the planned WSL store) caps an entry at this many bytes.
#[allow(dead_code)] // the WSL store that will use it is not implemented yet
pub const WSL_MAX_ENTRY_BYTES: usize = 2560;

#[derive(Serialize, Deserialize)]
pub struct StoredSecret {
    pub value: String,
    pub hosts: Vec<String>,
    pub env: String,
}

impl std::fmt::Debug for StoredSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSecret")
            .field("value", &"<redacted>")
            .field("hosts", &self.hosts)
            .field("env", &self.env)
            .finish()
    }
}

impl StoredSecret {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("StoredSecret serializes")
    }

    pub fn from_json(s: &str) -> Result<Self> {
        // Deliberately no error context carrying `s`: it holds the secret.
        // Fixed message only: serde's Display can echo the offending string.
        serde_json::from_str(s).map_err(|e| anyhow::anyhow!("invalid stored secret JSON ({:?} at line {} column {})", e.classify(), e.line(), e.column()))
    }
}

/// Names of the built-in GitHub secrets; neither store nor flag form may use them.
pub const RESERVED_NAMES: &[&str] = &["gh", "gh_basic"];

pub fn validate_name(name: &str) -> Result<()> {
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        bail!("invalid secret name {name:?}: use lowercase letters, digits, '-' or '_', starting with a letter");
    }
    if RESERVED_NAMES.contains(&name) {
        bail!("secret name {name:?} is reserved for the built-in GitHub secret");
    }
    Ok(())
}

pub fn parse_hosts(s: &str) -> Result<Vec<String>> {
    let mut hosts: Vec<String> = Vec::new();
    for h in s.split(',').map(str::trim).filter(|h| !h.is_empty()) {
        // boxlite matches bare host names only; a URL would save fine and never substitute.
        if h.contains('/') || h.contains(char::is_whitespace) {
            bail!("invalid host {h:?}: use a host name such as api.openai.com, not a URL");
        }
        if !hosts.iter().any(|x| x == h) {
            hosts.push(h.to_string());
        }
    }
    if hosts.is_empty() {
        bail!("at least one host is required (comma-separated)");
    }
    Ok(hosts)
}

pub fn validate_env(env: &str) -> Result<()> {
    let ok = env.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        bail!("invalid environment variable name {env:?}: use [A-Za-z_][A-Za-z0-9_]*");
    }
    Ok(())
}

pub fn check_size(json: &str, limit: Option<usize>) -> Result<()> {
    if let Some(limit) = limit
        && json.len() > limit
    {
        bail!("secret entry is {} bytes, over the {limit}-byte limit", json.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_accept_lowercase_digits_dash_underscore() {
        for n in ["a", "openai", "my-key_2", "a1"] {
            validate_name(n).unwrap();
        }
    }

    #[test]
    fn names_reject_uppercase_leading_digit_empty_and_gh() {
        for n in ["", "Abc", "1a", "-a", "_a", "a b", "gh", "gh_basic", "a.b"] {
            assert!(validate_name(n).is_err(), "{n:?}");
        }
    }

    #[test]
    fn hosts_are_trimmed_deduped_and_empties_dropped() {
        assert_eq!(parse_hosts(" a.com, b.com,,a.com ").unwrap(), ["a.com", "b.com"]);
    }

    #[test]
    fn urls_and_inner_whitespace_are_rejected_as_hosts() {
        for h in ["https://api.openai.com/v1", "api.openai.com/v1", "http://a.com", "a .com", "good.com,https://bad.com"] {
            let e = parse_hosts(h).unwrap_err().to_string();
            assert!(e.contains("not a URL"), "{h:?}: {e}");
        }
        assert!(parse_hosts("good.com,https://bad.com").unwrap_err().to_string().contains("https://bad.com"));
        assert_eq!(parse_hosts("a.com:8443,*.b.com").unwrap(), ["a.com:8443", "*.b.com"]);
    }

    #[test]
    fn empty_host_list_is_rejected() {
        assert!(parse_hosts("").is_err());
        assert!(parse_hosts(" , ,").is_err());
    }

    #[test]
    fn env_names() {
        validate_env("_A1").unwrap();
        assert!(validate_env("1A").is_err());
        assert!(validate_env("").is_err());
        assert!(validate_env("A-B").is_err());
    }

    #[test]
    fn debug_never_contains_the_value() {
        let s = StoredSecret { value: "sk-supersecret".into(), hosts: vec!["a.com".into()], env: "K".into() };
        let d = format!("{s:?}");
        assert!(!d.contains("sk-supersecret"));
        assert!(d.contains("<redacted>"));
    }

    #[test]
    fn from_json_error_never_echoes_value() {
        let err = StoredSecret::from_json(r#"{"value":"x","hosts":"sk-LEAKME","env":"E"}"#).unwrap_err();
        for out in [format!("{:?}", err), format!("{:#}", err)] {
            assert!(!out.contains("sk-LEAKME"), "leaked: {out}");
        }
    }

    #[test]
    fn json_round_trips() {
        let s = StoredSecret { value: "v\"€".into(), hosts: vec!["a.com".into(), "b.com".into()], env: "K".into() };
        let back = StoredSecret::from_json(&s.to_json()).unwrap();
        assert_eq!((back.value, back.hosts, back.env), (s.value, s.hosts, s.env));
    }

    #[test]
    fn size_counts_bytes_not_chars() {
        assert!(check_size(&"€".repeat(900), Some(WSL_MAX_ENTRY_BYTES)).unwrap_err().to_string().contains("2700"));
        check_size(&"x".repeat(2560), Some(WSL_MAX_ENTRY_BYTES)).unwrap();
        check_size(&"€".repeat(900), None).unwrap();
    }
}
