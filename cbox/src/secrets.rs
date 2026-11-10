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
    /// In-box variable carrying the placeholder; `None` only for a stored git-only secret.
    pub env_var: Option<String>,
    pub hosts: Vec<String>,
    pub value: Option<String>,
    pub stored: bool,
    /// Basic-auth username; set exactly when `git` is.
    pub user: Option<String>,
    /// Configure git in the box for every host, via a derived `<name>_basic` secret.
    pub git: bool,
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
            .field("user", &self.user)
            .field("git", &self.git)
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

    // Names ending in `_basic` are the derived copy of a stored `--git` secret.
    if name.ends_with(crate::secretstore::DERIVED_SUFFIX) {
        bail!("--secret name {name:?} is reserved: names ending in {:?} are used for a --git secret's encoded copy", crate::secretstore::DERIVED_SUFFIX);
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

    Ok(SecretSpec { name: name.to_string(), env_var: Some(env_var.to_string()), hosts, value: None, stored: false, user: None, git: false })
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
            let StoredSecret { value, hosts, env, user, git } =
                found?.ok_or_else(|| anyhow!("secret {name:?} not found; run cbox secret create {name}"))?;
            Ok(SecretSpec { name: name.to_string(), env_var: env, hosts, value: Some(value), stored: true, user, git })
        })
        .collect()
}

/// Two secrets writing the same in-box variable (or sharing a name, hence a
/// placeholder) would leave one silently shadowing the other; two `--git`
/// secrets for one host would fight over its `extraHeader`.
pub fn check_unique_env(specs: &[SecretSpec]) -> Result<()> {
    for (i, spec) in specs.iter().enumerate() {
        // One name means one placeholder, whatever the env vars are.
        if specs[..i].iter().any(|o| o.name == spec.name) {
            bail!("secret {:?} is given more than once; use each --secret name once", spec.name);
        }
        if let Some(var) = &spec.env_var
            && let Some(other) = specs[..i].iter().find(|o| o.env_var.as_ref() == Some(var))
        {
            bail!("--secret {} and --secret {} both set {var} in the box; use one", other.name, spec.name);
        }
        if spec.git
            && let Some((other, host)) = specs[..i].iter().filter(|o| o.git).find_map(|o| o.hosts.iter().find(|h| spec.hosts.contains(h)).map(|h| (o, h)))
        {
            bail!("--secret {} and --secret {} both configure git for {host} in the box; use one", other.name, spec.name);
        }
    }
    Ok(())
}

/// A variable counts as a usable source only if it is both set and non-empty.
fn set_and_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Read each spec's source variable and build the SDK secrets plus the
/// placeholder environment the guest receives.
pub fn build(specs: &[SecretSpec]) -> Result<Built> {
    let mut secrets = Vec::new();
    let mut env = Vec::new();
    let mut source_vars = Vec::new();

    for spec in specs {
        // Hosts are mandatory on every constructed secret: this is the
        // property the feature exists to provide, so it is enforced here
        // rather than trusted from the caller.
        if spec.hosts.is_empty() {
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
            None => {
                let var = spec.env_var.as_deref().ok_or_else(|| anyhow!("--secret {} has no value source", spec.name))?;
                set_and_non_empty(var).ok_or_else(|| anyhow!("--secret {} names {var}, which is unset or empty", spec.name))?
            }
        };

        secrets.push(Secret { name: spec.name.clone(), hosts: spec.hosts.clone(), placeholder: placeholder(&spec.name), value: value.clone() });
        if let Some(var) = &spec.env_var {
            source_vars.push(var.clone());
            env.push((var.clone(), placeholder(&spec.name)));
        }
        if spec.git {
            // git builds `Basic base64(user:token)` itself and base64 hides a placeholder from the
            // proxy, so the encoding moves to the host and the placeholder goes where the blob belongs.
            let user = spec.user.as_deref().ok_or_else(|| anyhow!("--secret {} has git on but no user", spec.name))?;
            let name = derived_name(&spec.name);
            secrets.push(Secret { placeholder: placeholder(&name), name, hosts: spec.hosts.clone(), value: BASE64.encode(format!("{user}:{value}")) });
        }
    }

    Ok(Built { secrets, env, source_vars })
}

/// The SDK secret holding `base64(user:token)` for a `--git` secret.
fn derived_name(name: &str) -> String {
    format!("{name}{}", crate::secretstore::DERIVED_SUFFIX)
}

/// The passthrough list minus the in-box names of stored secrets. A stored
/// value comes from the store, so the host variable of that name is not
/// forwarded and the guest gets the placeholder instead. Flag-form sources
/// stay, so `compose` still refuses them.
pub fn passthrough_without_stored(mut passthrough: Vec<String>, specs: &[SecretSpec]) -> Vec<String> {
    passthrough.retain(|k| !specs.iter().any(|s| s.stored && s.env_var.as_deref() == Some(k.as_str())));
    passthrough
}

/// Per-secret hashes recorded at creation and compared on resume. Flag-form
/// secrets keep the value-only hash so existing boxes show
/// no spurious "changed" warning; stored ones also cover hosts and env name.
pub fn resume_hashes(specs: &[SecretSpec], built: &Built) -> BTreeMap<String, u64> {
    let mut hashes: BTreeMap<String, u64> =
        built.secrets.iter().map(|s| (s.name.clone(), sidecar::hash_secret_value(&s.value))).collect();
    for spec in specs.iter().filter(|s| s.stored) {
        if let Some(value) = &spec.value {
            hashes.insert(spec.name.clone(), sidecar::hash_stored_secret(value, &spec.hosts, spec.env_var.as_deref(), spec.user.as_deref(), spec.git));
        }
    }
    hashes
}

/// Run inside the box at `up` when any secret has `--git`; `None` when none does.
///
/// For every host of every git secret: `http.https://<host>/.extraHeader` carries the placeholder
/// of the pre-encoded credential, and the credential helper is blanked. Blanking is required, not
/// tidiness: without it git falls back to a helper on a 401 and sends base64'd garbage.
///
/// The blank is scoped to `credential.https://<host>.helper`, not the generic `credential.helper`:
/// git resolves credential config by urlmatch specificity, not by which file it came from, so a
/// generic `--global` blank does not override a URL-scoped entry from a lower-precedence file.
/// Measured with git 2.50.1 via `git config --get-urlmatch credential https://github.com`.
///
/// Hosts are validated here too (they end up in a shell command), whatever stored them.
pub fn git_bootstrap_script(specs: &[SecretSpec]) -> Result<Option<String>> {
    let mut cmds = Vec::new();
    for spec in specs.iter().filter(|s| s.git) {
        let ph = placeholder(&derived_name(&spec.name));
        for host in &spec.hosts {
            crate::secretstore::validate_git_host(host)?;
            cmds.push(format!("git config --global 'http.https://{host}/.extraHeader' 'Authorization: Basic {ph}'"));
            cmds.push(format!("git config --global 'credential.https://{host}.helper' ''"));
        }
    }
    Ok((!cmds.is_empty()).then(|| cmds.join(" && ")))
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
        assert_eq!(placeholder("openai"), "<BOXLITE_SECRET:openai>");
        assert_eq!(placeholder("github_work_basic"), "<BOXLITE_SECRET:github_work_basic>");
    }

    #[test]
    fn parses_a_secret_flag_with_one_and_many_hosts() {
        let s = parse_secret_flag("openai=OPENAI_API_KEY@api.openai.com").unwrap();
        assert_eq!(s, SecretSpec {
            name: "openai".into(),
            env_var: Some("OPENAI_API_KEY".into()),
            hosts: vec!["api.openai.com".into()],
            value: None,
            stored: false,
            user: None,
            git: false,
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
    fn build_reports_a_missing_source_variable() {
        let _lock = crate::test_env_lock::lock();
        unsafe { std::env::remove_var("CBOX_T_MISSING") };
        let spec = SecretSpec {
            name: "x".into(),
            env_var: Some("CBOX_T_MISSING".into()),
            hosts: vec!["a.com".into()],
            value: None,
            stored: false,
            user: None,
            git: false,
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
            env_var: Some("CBOX_T_TOKEN".into()),
            hosts: vec!["a.com".into()],
            value: None,
            stored: false,
            user: None,
            git: false,
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

    /// `parse_secret_flag` already rejects an empty host list, but `build` is
    /// where `boxlite::Secret`s actually get constructed and handed across
    /// the SDK boundary — the "hosts are mandatory" invariant must not rest
    /// solely on caller discipline.
    #[test]
    fn build_rejects_a_hand_constructed_spec_with_no_hosts() {
        let spec = SecretSpec {
            name: "x".into(),
            env_var: Some("CBOX_T_NOHOSTS".into()),
            hosts: vec![],
            value: None,
            stored: false,
            user: None,
            git: false,
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
        assert!(specs_from_store(&st, &["one", "x_basic"]).is_err());
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

    fn spec(name: &str, env: &str, hosts: &[&str], value: Option<&str>) -> SecretSpec {
        SecretSpec {
            name: name.into(),
            env_var: Some(env.into()),
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            value: value.map(String::from),
            stored: value.is_some(),
            user: None,
            git: false,
        }
    }

    fn git_spec(name: &str, env: Option<&str>, hosts: &[&str], user: &str) -> SecretSpec {
        SecretSpec { env_var: env.map(String::from), user: Some(user.into()), git: true, ..spec(name, "UNUSED", hosts, Some("ghp_example")) }
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
        assert_eq!(base, sidecar::hash_stored_secret("v", &["a.com".to_string()], Some("E"), None, false));
        assert_ne!(base, hash(&["b.com"], "E"));
        assert_ne!(base, hash(&["a.com"], "F"));
    }


    #[test]
    fn a_flag_secret_named_with_the_derived_suffix_is_rejected() {
        let err = parse_secret_flag("gh_basic=X@a.com").unwrap_err().to_string();
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn a_git_secret_builds_the_raw_and_the_encoded_secret_on_the_same_hosts() {
        let hosts = ["github.com", "api.github.com", "gist.github.com"];
        let built = build(&[git_spec("github_work", Some("GH_TOKEN"), &hosts, "x-access-token")]).unwrap();
        assert_eq!(built.secrets.len(), 2);
        let raw = &built.secrets[0];
        assert_eq!((raw.name.as_str(), raw.value.as_str()), ("github_work", "ghp_example"));
        let basic = &built.secrets[1];
        assert_eq!((basic.name.as_str(), basic.placeholder.as_str()), ("github_work_basic", "<BOXLITE_SECRET:github_work_basic>"));
        assert_eq!(basic.hosts, hosts.map(String::from));
        assert_eq!(String::from_utf8(BASE64.decode(&basic.value).unwrap()).unwrap(), "x-access-token:ghp_example");
        // Only the raw placeholder is exposed as a variable.
        assert_eq!(built.env, vec![("GH_TOKEN".to_string(), "<BOXLITE_SECRET:github_work>".to_string())]);
        assert_eq!(built.source_vars, vec!["GH_TOKEN".to_string()]);
    }

    #[test]
    fn a_git_only_secret_sets_no_variable_and_an_empty_user_encodes_a_leading_colon() {
        let built = build(&[git_spec("azure", None, &["dev.azure.com"], "")]).unwrap();
        assert!(built.env.is_empty() && built.source_vars.is_empty());
        assert_eq!(String::from_utf8(BASE64.decode(&built.secrets[1].value).unwrap()).unwrap(), ":ghp_example");
    }

    #[test]
    fn a_stored_secret_without_env_loads_as_a_spec_with_no_variable() {
        let mut store = FakeStore::default();
        store.insert_git("gl", "tok", &["gitlab.com"], None, "oauth2");
        let spec = one(&store, "gl").unwrap();
        assert_eq!((spec.env_var, spec.user.as_deref(), spec.git), (None, Some("oauth2"), true));
    }

    #[test]
    fn the_bootstrap_writes_the_header_and_the_scoped_blank_for_every_git_host() {
        let specs = [git_spec("github_work", Some("GH_TOKEN"), &["github.com", "git.example.com:8443"], "x-access-token")];
        let script = git_bootstrap_script(&specs).unwrap().unwrap();
        for host in ["github.com", "git.example.com:8443"] {
            assert!(script.contains(&format!("git config --global 'http.https://{host}/.extraHeader' 'Authorization: Basic <BOXLITE_SECRET:github_work_basic>'")), "{script}");
            // Scoped to the URL, not the generic `credential.helper`: git resolves by urlmatch specificity.
            assert!(script.contains(&format!("git config --global 'credential.https://{host}.helper' ''")), "{script}");
        }
    }

    #[test]
    fn no_git_secret_means_no_bootstrap_and_a_hostile_host_is_refused() {
        assert_eq!(git_bootstrap_script(&[spec("k", "K", &["a.com"], Some("v"))]).unwrap(), None);
        let bad = [git_spec("x", None, &["a.com'; rm -rf /; '"], "u")];
        assert!(git_bootstrap_script(&bad).is_err());
    }

    #[test]
    fn two_git_secrets_on_one_host_are_rejected_but_disjoint_hosts_are_fine() {
        let a = git_spec("github_work", Some("GH_TOKEN"), &["github.com"], "u");
        let b = git_spec("github_personal", Some("GH_TOKEN2"), &["github.com", "gist.github.com"], "u");
        let err = check_unique_env(&[a.clone(), b]).unwrap_err().to_string();
        assert!(err.contains("github.com") && err.contains("git"), "{err}");
        let c = git_spec("gitlab", Some("GL_TOKEN"), &["gitlab.com"], "oauth2");
        check_unique_env(&[a, c]).unwrap();
    }

    #[test]
    fn git_only_secrets_do_not_collide_on_a_missing_variable() {
        let a = git_spec("a", None, &["a.com"], "u");
        let b = git_spec("b", None, &["b.com"], "u");
        check_unique_env(&[a, b]).unwrap();
    }

    #[test]
    fn resume_hash_of_a_git_secret_covers_user_and_git() {
        let h = |user: &str| {
            let mut s = git_spec("k", Some("E"), &["a.com"], user);
            s.value = Some("v".into());
            let specs = [s];
            resume_hashes(&specs, &build(&specs).unwrap())["k"]
        };
        assert_eq!(h("u"), h("u"));
        assert_ne!(h("u"), h("u2"));
    }
}
