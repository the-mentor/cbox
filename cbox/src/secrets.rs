//! Host-side credential substitution.
//!
//! GitHub needs two secrets because `gh` and `git` want the credential in
//! incompatible shapes. `gh` sends `Authorization: Bearer <placeholder>`, which
//! the proxy can match on the wire. `git` builds `Basic base64(user:token)`
//! itself, and base64 hides the placeholder from a literal string matcher —
//! measured against github.com, git-upload-pack returns 401 for Bearer and 200
//! for Basic, so Bearer is not an escape either. The fix is to move the base64
//! to the host: store the already-encoded credential as the value and put the
//! placeholder where the encoded blob belongs, via http.extraHeader, which git
//! passes through verbatim.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use boxlite::Secret;

use crate::secretstore::{SecretStore, StoredSecret};
use crate::sidecar;

/// The Basic-auth username GitHub expects when the password is a PAT.
pub const GH_BASIC_USER: &str = "x-access-token";

/// Must match the Python binding's format, or the same box would present
/// different placeholders depending on which SDK launched it.
pub fn placeholder(name: &str) -> String {
    format!("<BOXLITE_SECRET:{name}>")
}

/// `value` is `Some` only for a store-backed secret (`stored`); flag-form
/// secrets read `env_var` from the host in `build`.
#[derive(Clone, PartialEq)]
pub struct SecretSpec {
    pub name: String,
    pub env_var: String,
    pub hosts: Vec<String>,
    pub value: Option<String>,
    pub stored: bool,
}

// Hand-written so a stored value never reaches Debug output.
impl std::fmt::Debug for SecretSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretSpec")
            .field("name", &self.name)
            .field("env_var", &self.env_var)
            .field("hosts", &self.hosts)
            .field("value", &self.value.as_ref().map(|_| "<redacted>"))
            .field("stored", &self.stored)
            .finish()
    }
}

#[derive(Debug)]
pub struct Built {
    pub secrets: Vec<Secret>,
    pub env: Vec<(String, String)>,
    pub source_vars: Vec<String>,
}

/// `NAME=ENV_VAR@host[,host...]`. The flag names the variable; the value
/// never enters argv, where ps and shell history would capture it.
pub fn parse_secret_flag(s: &str) -> Result<SecretSpec> {
    let (lhs, hosts) = s
        .split_once('@')
        .ok_or_else(|| anyhow!("--secret needs @hosts: {s:?} (e.g. openai=OPENAI_API_KEY@api.openai.com)"))?;
    let (name, env_var) = lhs
        .split_once('=')
        .ok_or_else(|| anyhow!("--secret needs NAME=ENV_VAR: {s:?}"))?;

    if name.is_empty() || env_var.is_empty() {
        bail!("--secret needs a non-empty name and variable: {s:?}");
    }

    // "gh" is reserved: `secrets::build` special-cases a spec named "gh" as
    // the built-in GitHub preset and, in that branch, discards its
    // `hosts` entirely in favor of the two hardcoded GitHub hosts. A
    // user-supplied `--secret gh=ANYVAR@internal.example.com` would silently
    // hit that same branch and get retargeted at api.github.com/github.com
    // instead of the host the user actually named -- rejecting the name
    // outright is simpler and safer than trying to distinguish "the
    // preset" from "a user secret that happens to be named gh" downstream.
    // "gh_basic" is reserved too: it is the GitHub preset's second SDK secret.
    if crate::secretstore::RESERVED_NAMES.contains(&name) {
        bail!(
            "--secret name {name:?} is reserved for the built-in GitHub secret \
             (auto-detected from GH_TOKEN/GITHUB_TOKEN); choose a different name."
        );
    }

    let hosts: Vec<String> = hosts
        .split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(String::from)
        .collect();
    if hosts.is_empty() {
        bail!("--secret {name} has no hosts. An unscoped secret is substituted on requests to any host.");
    }

    Ok(SecretSpec { name: name.to_string(), env_var: env_var.to_string(), hosts, value: None, stored: false })
}

/// A bare `--secret NAME` (no `=`, no `@`) names a secret in the store.
pub fn is_store_reference(flag: &str) -> bool {
    !flag.contains('=') && !flag.contains('@')
}

/// Specs for `--secret` flags, in flag order. Full-form flags are parsed first, so their errors
/// come before the store is opened; `open` runs only when a bare name exists.
pub fn specs_from_flags(flags: &[String], open: impl FnOnce() -> Result<Box<dyn SecretStore>>) -> Result<Vec<SecretSpec>> {
    let mut slots = Vec::new();
    for f in flags {
        slots.push(if is_store_reference(f) { None } else { Some(parse_secret_flag(f)?) });
    }
    let bare: Vec<&str> = flags.iter().map(String::as_str).filter(|f| is_store_reference(f)).collect();
    let mut stored = if bare.is_empty() { Vec::new() } else { specs_from_store(open()?.as_ref(), &bare)? }.into_iter();
    slots.into_iter().map(|s| s.or_else(|| stored.next()).ok_or_else(|| anyhow!("fewer stored secrets than --secret names"))).collect()
}

/// Look the names up in the store with one batched read. A missing name is an error, never a
/// fallback to a host variable: the user asked for the stored secret. Errors come in name order.
pub fn specs_from_store(store: &dyn SecretStore, names: &[&str]) -> Result<Vec<SecretSpec>> {
    for name in names {
        crate::secretstore::validate_name(name)?;
    }
    let results = store.get_many(names);
    if results.len() != names.len() {
        bail!("secret store returned {} results for {} names", results.len(), names.len());
    }
    names
        .iter()
        .zip(results)
        .map(|(name, found)| {
            let StoredSecret { value, hosts, env } =
                found?.ok_or_else(|| anyhow!("secret {name:?} not found; run cbox secret create {name}"))?;
            Ok(SecretSpec { name: name.to_string(), env_var: env, hosts, value: Some(value), stored: true })
        })
        .collect()
}

/// Two secrets writing the same in-box variable (or sharing a name, hence a
/// placeholder) would leave one silently shadowing the other.
pub fn check_unique_env(specs: &[SecretSpec]) -> Result<()> {
    for (i, spec) in specs.iter().enumerate() {
        // One name means one placeholder, whatever the env vars are.
        if specs[..i].iter().any(|o| o.name == spec.name) {
            bail!("secret {:?} is given more than once; use each --secret name once", spec.name);
        }
        if let Some(other) = specs[..i].iter().find(|o| o.env_var == spec.env_var) {
            let who = |s: &SecretSpec| {
                if s.name == "gh" { "the GitHub token (GH_TOKEN/GITHUB_TOKEN)".to_string() } else { format!("--secret {}", s.name) }
            };
            bail!("{} and {} both set {} in the box; use one", who(other), who(spec), spec.env_var);
        }
    }
    Ok(())
}

/// A variable counts as a usable source only if it is both set and non-empty.
/// `github_spec` and `build` must agree on this predicate: picking a variable
/// here that `build` would then reject as empty produces a misleading error
/// pointing at the wrong variable.
fn set_and_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Whether GitHub secret support should be requested at all. Built on the
/// same `set_and_non_empty` predicate as `github_spec`/`build`, so it cannot
/// drift from what those actually treat as usable: in particular,
/// `GH_TOKEN=""` (e.g. a blank line in a sourced `.env`) must not count as
/// present when `GITHUB_TOKEN` is also unset, or a caller that gates on this
/// and then calls `build` unconditionally would abort on a spurious "unset or
/// empty" error instead of silently skipping the secret, as having neither
/// variable set at all does.
pub fn has_github_token() -> bool {
    set_and_non_empty("GH_TOKEN").is_some() || set_and_non_empty("GITHUB_TOKEN").is_some()
}

/// The GitHub preset. Its `name` is a marker: `build` expands it into the two
/// secrets the two protocols need, using its own hardcoded host lists rather
/// than this spec's `hosts` -- which is therefore left empty here rather
/// than populated with a value nothing reads, so there's no data sitting
/// around that looks live but isn't. `parse_secret_flag` rejects a
/// user-supplied `--secret` named "gh" outright, so this is the only
/// constructor that ever produces one.
pub fn github_spec() -> SecretSpec {
    let env_var = if set_and_non_empty("GH_TOKEN").is_some() { "GH_TOKEN" } else { "GITHUB_TOKEN" };
    SecretSpec { name: "gh".into(), env_var: env_var.into(), hosts: vec![], value: None, stored: false }
}

/// Read each spec's source variable and build the SDK secrets plus the
/// placeholder environment the guest receives.
pub fn build(specs: &[SecretSpec]) -> Result<Built> {
    let mut secrets = Vec::new();
    let mut env = Vec::new();
    let mut source_vars = Vec::new();

    for spec in specs {
        // Hosts are mandatory on every constructed secret — this is the
        // property the feature exists to provide, so it is enforced here
        // rather than trusted from the caller. The "gh" marker is exempt: it
        // hardcodes its own two host lists below instead of using spec.hosts.
        if spec.name != "gh" && spec.hosts.is_empty() {
            bail!(
                "--secret {} has no hosts. An unscoped secret is substituted on requests to any host.",
                spec.name
            );
        }

        // For a stored secret `env_var` is the in-box name only; it is still
        // pushed to `source_vars` so compose refuses `-e` of that name (the
        // passthrough copy is dropped by `passthrough_without_stored`).
        let value = match &spec.value {
            Some(v) => v.clone(),
            None => set_and_non_empty(&spec.env_var)
                .ok_or_else(|| anyhow!("--secret {} names {}, which is unset or empty", spec.name, spec.env_var))?,
        };

        source_vars.push(spec.env_var.clone());

        if spec.name == "gh" {
            // Two secrets: gh sends Bearer to api.github.com; git sends Basic
            // to github.com and encodes it itself, so the encoding moves here.
            secrets.push(Secret {
                name: "gh".into(),
                hosts: vec!["api.github.com".into()],
                placeholder: placeholder("gh"),
                value: value.clone(),
            });
            secrets.push(Secret {
                name: "gh_basic".into(),
                hosts: vec!["github.com".into()],
                placeholder: placeholder("gh_basic"),
                value: BASE64.encode(format!("{GH_BASIC_USER}:{value}")),
            });
            env.push((spec.env_var.clone(), placeholder("gh")));
        } else {
            secrets.push(Secret {
                name: spec.name.clone(),
                hosts: spec.hosts.clone(),
                placeholder: placeholder(&spec.name),
                value,
            });
            env.push((spec.env_var.clone(), placeholder(&spec.name)));
        }
    }

    Ok(Built { secrets, env, source_vars })
}

/// The passthrough list minus the in-box names of stored secrets. A stored
/// value comes from the store, so the host variable of that name is not
/// forwarded and the guest gets the placeholder instead. Flag-form sources
/// stay, so `compose` still refuses them.
pub fn passthrough_without_stored(mut passthrough: Vec<String>, specs: &[SecretSpec]) -> Vec<String> {
    passthrough.retain(|k| !specs.iter().any(|s| s.stored && s.env_var == *k));
    passthrough
}

/// Per-secret hashes recorded at creation and compared on resume. Flag-form
/// (and gh/gh_basic) secrets keep the value-only hash so existing boxes show
/// no spurious "changed" warning; stored ones also cover hosts and env name.
pub fn resume_hashes(specs: &[SecretSpec], built: &Built) -> BTreeMap<String, u64> {
    let mut hashes: BTreeMap<String, u64> =
        built.secrets.iter().map(|s| (s.name.clone(), sidecar::hash_secret_value(&s.value))).collect();
    for spec in specs.iter().filter(|s| s.stored) {
        if let Some(value) = &spec.value {
            hashes.insert(spec.name.clone(), sidecar::hash_stored_secret(value, &spec.hosts, &spec.env_var));
        }
    }
    hashes
}

/// Run inside the box after creation when GitHub secrets are present.
///
/// Blanking the helper is required, not tidiness: without it git falls back to
/// it on a 401 and sends base64'd garbage, which fails confusingly.
///
/// The blank must be scoped to `credential.https://github.com.helper`, not
/// the generic `credential.helper` — git resolves credential config by
/// urlmatch specificity, not by which file it came from, so a generic
/// `--global` blank does not override a URL-scoped entry from a lower-
/// precedence file (there is no such entry baked into the image anymore, but
/// a scoped blank is also what correctly overrides one if some other layer
/// ever adds it back). Measured with git 2.50.1 via
/// `git config --get-urlmatch credential https://github.com`.
pub fn git_bootstrap_script() -> String {
    format!(
        "git config --global http.https://github.com/.extraHeader \
         'Authorization: Basic {}' && \
         git config --global credential.https://github.com.helper ''",
        placeholder("gh_basic")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Panic-safe env var mutation for tests lives in `test_env_lock`, shared
    // with every other module whose tests touch the environment. Binding the
    // guard to `_` still drops it immediately, so every call site below
    // binds it to a named local.
    use crate::test_env_lock::EnvVarGuard as EnvGuard;

    #[test]
    fn placeholder_matches_the_python_binding_format() {
        assert_eq!(placeholder("gh"), "<BOXLITE_SECRET:gh>");
        assert_eq!(placeholder("gh_basic"), "<BOXLITE_SECRET:gh_basic>");
    }

    #[test]
    fn parses_a_secret_flag_with_one_and_many_hosts() {
        let s = parse_secret_flag("openai=OPENAI_API_KEY@api.openai.com").unwrap();
        assert_eq!(s, SecretSpec {
            name: "openai".into(),
            env_var: "OPENAI_API_KEY".into(),
            hosts: vec!["api.openai.com".into()],
            value: None,
            stored: false,
        });

        let s = parse_secret_flag("x=X@a.com,b.com").unwrap();
        assert_eq!(s.hosts, vec!["a.com".to_string(), "b.com".to_string()]);
    }

    #[test]
    fn a_secret_without_hosts_is_rejected() {
        // An unscoped secret is substituted on requests to ANY host, which
        // inverts the property the feature exists to provide.
        assert!(parse_secret_flag("openai=OPENAI_API_KEY").is_err());
        assert!(parse_secret_flag("openai=OPENAI_API_KEY@").is_err());
    }

    #[test]
    fn a_secret_without_an_env_var_is_rejected() {
        assert!(parse_secret_flag("openai@api.openai.com").is_err());
    }

    #[test]
    fn a_secret_named_gh_basic_is_rejected_as_reserved() {
        let err = parse_secret_flag("gh_basic=X@a.com").unwrap_err().to_string();
        assert!(err.contains("reserved for the built-in GitHub secret"), "{err}");
    }

    #[test]
    fn duplicate_env_with_the_github_preset_names_the_token() {
        let _lock = crate::test_env_lock::lock();
        let _g = EnvGuard::set("GH_TOKEN", "t");
        let specs = vec![github_spec(), parse_secret_flag("x=GH_TOKEN@a.com").unwrap()];
        let err = check_unique_env(&specs).unwrap_err().to_string();
        assert!(err.contains("the GitHub token (GH_TOKEN/GITHUB_TOKEN)"), "{err}");
        assert!(!err.contains("--secret gh"), "{err}");
    }

    /// "gh" is reserved for the built-in GitHub preset. Without this, a
    /// user-supplied `--secret gh=ANYVAR@internal.example.com` would hit
    /// `build`'s "gh" special case and get silently retargeted at
    /// api.github.com/github.com instead of the host actually named.
    #[test]
    fn a_secret_named_gh_is_rejected_as_reserved() {
        let err = parse_secret_flag("gh=ANYVAR@internal.example.com").unwrap_err().to_string();
        assert!(err.contains("gh"), "names the reserved name: {err}");
        assert!(err.contains("reserved"), "explains why: {err}");
    }

    #[test]
    fn build_reports_a_missing_source_variable() {
        let _lock = crate::test_env_lock::lock();
        unsafe { std::env::remove_var("CBOX_T_MISSING") };
        let spec = SecretSpec {
            name: "x".into(),
            env_var: "CBOX_T_MISSING".into(),
            hosts: vec!["a.com".into()],
            value: None,
            stored: false,
        };
        let err = build(&[spec]).unwrap_err().to_string();
        assert!(err.contains("CBOX_T_MISSING"), "names the variable: {err}");
    }

    #[test]
    fn build_sets_the_guest_variable_to_the_placeholder_not_the_value() {
        let _lock = crate::test_env_lock::lock();
        let _guard = EnvGuard::set("CBOX_T_TOKEN", "super-secret");
        let spec = SecretSpec {
            name: "mysecret".into(),
            env_var: "CBOX_T_TOKEN".into(),
            hosts: vec!["a.com".into()],
            value: None,
            stored: false,
        };
        let built = build(&[spec]).unwrap();
        assert_eq!(
            built.env,
            vec![("CBOX_T_TOKEN".to_string(), "<BOXLITE_SECRET:mysecret>".to_string())]
        );
        assert_eq!(built.source_vars, vec!["CBOX_T_TOKEN".to_string()]);
        // The real value must never appear in what the guest receives.
        assert!(!built.env.iter().any(|(_, v)| v.contains("super-secret")));
    }

    /// `has_github_token` must agree with `github_spec`/`build` on what counts
    /// as usable, or a caller gating on it can still hit `build`'s "unset or
    /// empty" error. In particular `GH_TOKEN=""` with no `GITHUB_TOKEN` must
    /// read as absent, exactly like having neither variable set at all — this
    /// is the exact bug this function exists to prevent.
    #[test]
    fn has_github_token_treats_a_blank_gh_token_as_absent() {
        let _lock = crate::test_env_lock::lock();
        unsafe { std::env::remove_var("GH_TOKEN") };
        unsafe { std::env::remove_var("GITHUB_TOKEN") };
        assert!(!has_github_token(), "both unset");

        let _gh = EnvGuard::set("GH_TOKEN", "");
        assert!(!has_github_token(), "GH_TOKEN=\"\" with GITHUB_TOKEN unset");

        let _ghub = EnvGuard::set("GITHUB_TOKEN", "ghp_from_github_token");
        assert!(has_github_token(), "GH_TOKEN=\"\" with a valid GITHUB_TOKEN");
    }

    #[test]
    fn github_produces_two_secrets_scoped_to_different_hosts() {
        let _lock = crate::test_env_lock::lock();
        let _guard = EnvGuard::set("GH_TOKEN", "ghp_example");
        let built = build(&[github_spec()]).unwrap();
        assert_eq!(built.secrets.len(), 2);

        let gh = built.secrets.iter().find(|s| s.name == "gh").unwrap();
        assert_eq!(gh.hosts, vec!["api.github.com".to_string()]);
        assert_eq!(gh.value, "ghp_example");

        let basic = built.secrets.iter().find(|s| s.name == "gh_basic").unwrap();
        assert_eq!(basic.hosts, vec!["github.com".to_string()]);
    }

    /// `GH_TOKEN` set-but-empty must not shadow a perfectly usable
    /// `GITHUB_TOKEN` — `github_spec` and `build` must agree on what "usable"
    /// means, or this fails with a misleading error about the wrong variable.
    #[test]
    fn github_prefers_github_token_when_gh_token_is_set_but_empty() {
        let _lock = crate::test_env_lock::lock();
        let _gh = EnvGuard::set("GH_TOKEN", "");
        let _ghub = EnvGuard::set("GITHUB_TOKEN", "ghp_from_github_token");

        let spec = github_spec();
        assert_eq!(spec.env_var, "GITHUB_TOKEN");

        let built = build(&[spec]).unwrap();
        assert_eq!(built.source_vars, vec!["GITHUB_TOKEN".to_string()]);
        let gh = built.secrets.iter().find(|s| s.name == "gh").unwrap();
        assert_eq!(gh.value, "ghp_from_github_token");
    }

    /// The whole git fix rests on this, and it is the one value never
    /// eyeballed in any output.
    #[test]
    fn the_git_blob_decodes_to_what_github_expects() {
        let _lock = crate::test_env_lock::lock();
        let _guard = EnvGuard::set("GH_TOKEN", "ghp_example");
        let built = build(&[github_spec()]).unwrap();
        let basic = built.secrets.iter().find(|s| s.name == "gh_basic").unwrap();

        let decoded = String::from_utf8(BASE64.decode(&basic.value).unwrap()).unwrap();
        assert_eq!(decoded, "x-access-token:ghp_example");
        // And the placeholder must NOT survive encoding — that asymmetry is
        // precisely why the credential-helper path cannot work and this can.
        assert!(!basic.value.contains("x-access-token"));
    }

    /// `parse_secret_flag` already rejects an empty host list, but `build` is
    /// where `boxlite::Secret`s actually get constructed and handed across
    /// the SDK boundary — the "hosts are mandatory" invariant must not rest
    /// solely on caller discipline.
    #[test]
    fn build_rejects_a_hand_constructed_spec_with_no_hosts() {
        let spec = SecretSpec {
            name: "x".into(),
            env_var: "CBOX_T_NOHOSTS".into(),
            hosts: vec![],
            value: None,
            stored: false,
        };
        let err = build(&[spec]).unwrap_err().to_string();
        assert!(err.contains("no hosts"), "names the problem: {err}");
    }

    use crate::secretstore::testing::FakeStore;

    fn one(s: &dyn SecretStore, n: &str) -> Result<SecretSpec> {
        Ok(specs_from_store(s, &[n])?.remove(0))
    }

    #[test]
    fn specs_from_store_batches_and_reports_the_first_missing_name() {
        let st = FakeStore::with("one", "v", &["a.com"], "K1");
        let specs = specs_from_store(&st, &["one"]).unwrap();
        assert_eq!((specs[0].name.as_str(), st.get_many_calls()), ("one", 1));
        let err = specs_from_store(&st, &["one", "x"]).unwrap_err().to_string();
        assert_eq!((err.as_str(), st.get_many_calls()), ("secret \"x\" not found; run cbox secret create x", 2));
        assert!(specs_from_store(&st, &["one", "gh"]).is_err());
        assert_eq!(st.get_many_calls(), 2, "validation happens before any read");
    }

    #[test]
    fn flag_form_errors_come_before_the_store_is_opened() {
        let flags = ["openai".to_string(), "foo=BAR".to_string()];
        let e = specs_from_flags(&flags, || bail!("no secret store available")).unwrap_err().to_string();
        assert!(e.contains("needs @hosts"), "{e}");
        // No bare names: the store is never opened.
        let flags = ["a=K@h.com".to_string()];
        assert_eq!(specs_from_flags(&flags, || bail!("opened")).unwrap().len(), 1);
    }

    #[test]
    fn specs_from_flags_keeps_flag_order_with_bare_and_full_forms() {
        let flags = ["x=K2@b.com".to_string(), "mine".to_string(), "y=K3@c.com".to_string()];
        let specs = specs_from_flags(&flags, || Ok(Box::new(FakeStore::with("mine", "v", &["a.com"], "K1")))).unwrap();
        assert_eq!(specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["x", "mine", "y"]);
    }

    #[test]
    fn specs_from_store_errors_on_a_result_count_mismatch() {
        let e = specs_from_store(&FakeStore::short(), &["one"]).unwrap_err().to_string();
        assert_eq!(e, "secret store returned 0 results for 1 names");
    }

    #[test]
    fn bare_name_is_a_store_reference_and_full_form_is_not() {
        assert!(is_store_reference("openai"));
        assert!(!is_store_reference("openai=OPENAI_API_KEY@api.openai.com"));
        assert!(!is_store_reference("openai=OPENAI_API_KEY"));
        assert!(!is_store_reference("openai@api.openai.com"));
    }

    #[test]
    fn missing_store_secret_fails_naming_create() {
        let store = FakeStore::with("other", "v", &["a.com"], "OTHER");
        let err = one(&store, "x").unwrap_err().to_string();
        assert_eq!(err, "secret \"x\" not found; run cbox secret create x");
    }

    #[test]
    fn duplicate_secret_names_are_rejected_even_with_different_env() {
        let store = FakeStore::with("openai", "v", &["a.com"], "OPENAI_API_KEY");
        let specs = vec![one(&store, "openai").unwrap(), parse_secret_flag("openai=OTHER@b.com").unwrap()];
        let err = check_unique_env(&specs).unwrap_err().to_string();
        assert!(err.contains("openai") && err.contains("more than once"), "{err}");
    }

    #[test]
    fn gh_bare_name_is_reserved() {
        let store = FakeStore::with("gh", "v", &["a.com"], "GH_TOKEN");
        let err = one(&store, "gh").unwrap_err().to_string();
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn build_uses_the_stored_value_and_returns_a_placeholder_in_env() {
        let _lock = crate::test_env_lock::lock();
        // A host variable of the same name must not be read instead.
        let _guard = EnvGuard::remove("CBOX_T_STORED");
        let store = FakeStore::with("mine", "sk-stored-value", &["a.com", "b.com"], "CBOX_T_STORED");
        let spec = one(&store, "mine").unwrap();
        assert!(spec.stored);
        assert!(!format!("{spec:?}").contains("sk-stored-value"), "Debug leaks the value");

        let built = build(&[spec]).unwrap();
        assert_eq!(built.secrets.len(), 1);
        assert_eq!(built.secrets[0].value, "sk-stored-value");
        assert_eq!(built.secrets[0].hosts, vec!["a.com".to_string(), "b.com".to_string()]);
        assert_eq!(built.env, vec![("CBOX_T_STORED".to_string(), "<BOXLITE_SECRET:mine>".to_string())]);
        assert_eq!(built.source_vars, vec!["CBOX_T_STORED".to_string()]);
    }

    #[test]
    fn store_secret_env_name_is_excluded_from_passthrough() {
        let _lock = crate::test_env_lock::lock();
        let _guard = EnvGuard::set("ANTHROPIC_API_KEY", "host-MARKER-value");
        let store = FakeStore::with("anthropic", "sk-stored", &["api.anthropic.com"], "ANTHROPIC_API_KEY");
        let specs = [one(&store, "anthropic").unwrap()];
        let built = build(&specs).unwrap();
        let passthrough = vec!["ANTHROPIC_API_KEY".to_string(), "ANTHROPIC_MODEL".to_string()];
        let passthrough = passthrough_without_stored(passthrough, &specs);
        assert_eq!(passthrough, ["ANTHROPIC_MODEL"]);
        let plain = crate::env::compose(&[], &passthrough, &built.source_vars).unwrap();
        assert!(!plain.iter().any(|(_, v)| v.contains("MARKER")), "host value leaked: {plain:?}");

        // An explicit -e for the same name still fails closed.
        let err = crate::env::compose(&["ANTHROPIC_API_KEY=x".to_string()], &passthrough, &built.source_vars)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot also be set with -e or in a profile's env"), "{err}");
    }

    #[test]
    fn flag_secret_source_on_the_passthrough_list_still_errors() {
        let _lock = crate::test_env_lock::lock();
        let _guard = EnvGuard::set("ANTHROPIC_API_KEY", "host-value");
        let specs = [parse_secret_flag("anthropic=ANTHROPIC_API_KEY@api.anthropic.com").unwrap()];
        let built = build(&specs).unwrap();
        let passthrough = passthrough_without_stored(vec!["ANTHROPIC_API_KEY".to_string()], &specs);
        let err = crate::env::compose(&[], &passthrough, &built.source_vars).unwrap_err().to_string();
        assert!(err.contains("named as a secret source"), "{err}");
    }

    #[test]
    fn store_secret_env_name_collides_with_dash_e() {
        let store = FakeStore::with("mine", "sk-stored", &["a.com"], "CBOX_T_DASH_E");
        let built = build(&[one(&store, "mine").unwrap()]).unwrap();
        let err = crate::env::compose(&["CBOX_T_DASH_E=x".to_string()], &[], &built.source_vars)
            .unwrap_err()
            .to_string();
        assert!(err.contains("named as a secret source"), "{err}");
    }

    #[test]
    fn two_store_secrets_with_one_env_name_error() {
        let mut store = FakeStore::with("one", "v1", &["a.com"], "SAME_VAR");
        store.insert("two", "v2", &["b.com"], "SAME_VAR");
        let specs = vec![one(&store, "one").unwrap(), one(&store, "two").unwrap()];
        let err = check_unique_env(&specs).unwrap_err().to_string();
        assert!(err.contains("SAME_VAR"), "{err}");

        // Mixed with the flag form too.
        let specs = vec![
            one(&store, "one").unwrap(),
            parse_secret_flag("flag=SAME_VAR@c.com").unwrap(),
        ];
        assert!(check_unique_env(&specs).is_err());

        let specs = vec![one(&store, "one").unwrap(), parse_secret_flag("flag=OTHER@c.com").unwrap()];
        check_unique_env(&specs).unwrap();
    }

    #[test]
    fn the_bootstrap_blanks_the_credential_helper() {
        let script = git_bootstrap_script();
        assert!(script.contains("<BOXLITE_SECRET:gh_basic>"));
        assert!(script.contains("extraHeader"));
        // Without this, git falls back to the helper on a 401 and sends
        // base64'd garbage. It must be scoped to the same URL
        // (`credential.https://github.com.helper`), not the generic
        // `credential.helper` -- git resolves credential config by urlmatch
        // specificity, so a generic blank does not override a URL-scoped
        // entry from another config file. Measured live with
        // `git config --get-urlmatch credential https://github.com`.
        assert!(
            script.contains("credential.https://github.com.helper"),
            "the blank must be scoped to the same URL, not the generic key: {script}"
        );
    }

    fn spec(name: &str, env: &str, hosts: &[&str], value: Option<&str>) -> SecretSpec {
        SecretSpec {
            name: name.into(),
            env_var: env.into(),
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            value: value.map(String::from),
            stored: value.is_some(),
        }
    }

    #[test]
    fn resume_hash_of_a_flag_secret_is_value_only() {
        let _lock = crate::test_env_lock::lock();
        let _g = EnvGuard::set("CBOX_T_RESUME", "v1");
        let specs = [spec("k", "CBOX_T_RESUME", &["a.com"], None)];
        let h = resume_hashes(&specs, &build(&specs).unwrap());
        assert_eq!(h["k"], sidecar::hash_secret_value("v1"));
    }

    #[test]
    fn resume_hash_of_a_stored_secret_covers_hosts_and_env() {
        let hash = |hosts: &[&str], env: &str| {
            let specs = [spec("k", env, hosts, Some("v"))];
            resume_hashes(&specs, &build(&specs).unwrap())["k"]
        };
        let base = hash(&["a.com"], "E");
        assert_eq!(base, sidecar::hash_stored_secret("v", &["a.com".to_string()], "E"));
        assert_ne!(base, hash(&["b.com"], "E"));
        assert_ne!(base, hash(&["a.com"], "F"));
    }

    #[test]
    fn resume_hashes_for_gh_keep_both_entries_value_only() {
        let _lock = crate::test_env_lock::lock();
        let _g = EnvGuard::set("GH_TOKEN", "ghp_x");
        let specs = [github_spec()];
        let built = build(&specs).unwrap();
        let h = resume_hashes(&specs, &built);
        assert_eq!(h.len(), 2);
        for s in &built.secrets {
            assert_eq!(h[&s.name], sidecar::hash_secret_value(&s.value));
        }
    }
}
