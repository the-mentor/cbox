//! Pure data rules for stored secrets: names, hosts, env vars, JSON form, size limit.

use anyhow::{Result, bail};

mod index;
mod service;
mod store;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(target_os = "linux")]
mod wincred;
pub use index::Index;
pub use service::{EditChanges, EditOutcome, HostChange, ListRow, RowState, check_absent, check_editable, create, delete, edit, exists, list};
pub use store::{SecretStore, open_default};
use serde::{Deserialize, Serialize};

/// Windows Credential Manager (the WSL store) caps an entry at this many bytes.
#[cfg_attr(target_os = "macos", allow(dead_code))] // only the Linux WSL store uses it
pub const WSL_MAX_ENTRY_BYTES: usize = 2560;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct StoredSecret {
    pub value: String,
    pub hosts: Vec<String>,
    /// In-box variable that carries the placeholder; `None` for a git-only secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Basic-auth username for the derived `base64(user:token)`; set exactly when `git` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Configure git in the box to authenticate to every host with this secret.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub git: bool,
}

impl std::fmt::Debug for StoredSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSecret")
            .field("value", &"<redacted>")
            .field("hosts", &self.hosts)
            .field("env", &self.env)
            .field("user", &self.user)
            .field("git", &self.git)
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

/// Suffix of the SDK secret derived from a `--git` secret (`<name>_basic`); no user secret may end in it.
pub const DERIVED_SUFFIX: &str = "_basic";

/// A lowercase letter, then lowercase letters, digits, '-' or '_'. Must match the regex in wincred.ps1.
pub(super) fn name_charset_ok(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn validate_name(name: &str) -> Result<()> {
    if !name_charset_ok(name) {
        bail!("invalid secret name {name:?}: use lowercase letters, digits, '-' or '_', starting with a letter");
    }
    if name.ends_with(DERIVED_SUFFIX) {
        bail!("secret name {name:?} is reserved: names ending in {DERIVED_SUFFIX:?} are used for the encoded copy of a --git secret");
    }
    Ok(())
}

/// A secret must reach the box somehow, and `--git` needs a username (which may be empty, e.g. Azure DevOps).
pub fn validate_shape(s: &StoredSecret) -> Result<()> {
    if s.env.is_none() && !s.git {
        bail!("a secret needs --env, --git, or both: with neither it would never reach the box");
    }
    match (s.git, &s.user) {
        (true, None) => bail!("--git needs --user (the Basic-auth username, e.g. x-access-token for GitHub; may be empty)"),
        (false, Some(_)) => bail!("--user only applies with --git"),
        _ => {}
    }
    if s.git {
        for h in &s.hosts {
            validate_git_host(h)?;
        }
    }
    Ok(())
}

/// Hosts go into a shell command and a git config key: a plain name with an optional port, nothing else.
pub fn validate_git_host(h: &str) -> Result<()> {
    let ok = !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    if !ok {
        bail!("invalid host {h:?} for --git: use letters, digits, '.', '-' and an optional :port");
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
        bail!("the stored entry is {} bytes (value plus hosts and env name), over the {limit}-byte limit", json.len());
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
        for n in ["", "Abc", "1a", "-a", "_a", "a b", "gh_basic", "a_basic", "a.b"] {
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
        let s = StoredSecret { value: "sk-supersecret".into(), hosts: vec!["a.com".into()], env: Some("K".into()), ..Default::default() };
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
        let s = StoredSecret { value: "v\"€".into(), hosts: vec!["a.com".into(), "b.com".into()], env: Some("K".into()), user: Some("u".into()), git: true };
        let back = StoredSecret::from_json(&s.to_json()).unwrap();
        assert_eq!((back.value, back.hosts, back.env, back.user, back.git), (s.value, s.hosts, s.env, s.user, s.git));
    }

    #[test]
    fn size_counts_bytes_not_chars() {
        assert!(check_size(&"€".repeat(900), Some(WSL_MAX_ENTRY_BYTES)).unwrap_err().to_string().contains("2700"));
        check_size(&"x".repeat(2560), Some(WSL_MAX_ENTRY_BYTES)).unwrap();
        check_size(&"€".repeat(900), None).unwrap();
    }

    #[test]
    fn an_old_entry_without_user_or_git_still_loads_and_a_plain_one_serializes_as_before() {
        let old = StoredSecret::from_json(r#"{"value":"v","hosts":["a.com"],"env":"K"}"#).unwrap();
        assert_eq!((old.env.as_deref(), old.user, old.git), (Some("K"), None, false));
        let plain = StoredSecret { value: "v".into(), hosts: vec!["a.com".into()], env: Some("K".into()), ..Default::default() };
        assert_eq!(plain.to_json(), r#"{"value":"v","hosts":["a.com"],"env":"K"}"#);
    }

    #[test]
    fn shape_needs_env_or_git_and_a_user_exactly_with_git() {
        let base = || StoredSecret { value: "v".into(), hosts: vec!["a.com".into()], ..Default::default() };
        assert!(validate_shape(&base()).is_err());
        validate_shape(&StoredSecret { env: Some("K".into()), ..base() }).unwrap();
        assert!(validate_shape(&StoredSecret { git: true, ..base() }).unwrap_err().to_string().contains("--user"));
        validate_shape(&StoredSecret { git: true, user: Some(String::new()), ..base() }).unwrap();
        assert!(validate_shape(&StoredSecret { env: Some("K".into()), user: Some("u".into()), ..base() }).is_err());
    }

    #[test]
    fn git_hosts_reject_shell_and_config_metacharacters() {
        for h in ["a.com;rm", "a.com'", "a b", "a.com/x", "$(x)", ""] {
            assert!(validate_git_host(h).is_err(), "{h:?}");
        }
        for h in ["github.com", "git.example.com:8443", "10.0.0.1"] {
            validate_git_host(h).unwrap();
        }
    }
}
