//! Egress policy: `--allow-net` / `--network disabled` -> BoxLite `NetworkSpec`.
//!
//! Pure logic, no runtime calls. BoxLite 0.10.4 matches IP/CIDR rules on the
//! destination address and hostname rules on the TLS SNI / HTTP Host; DNS is
//! not filtered, and hosts covered by a configured `Secret` stay reachable on
//! :443 regardless of the list. See docs/design/allow-net.md.

use anyhow::{Result, bail};
use boxlite::{NetworkInfo, NetworkMode, NetworkSpec, Secret};
use std::net::IpAddr;

/// `host.boxlite.internal`. Added by IP, not name: Claude reaches the gateway
/// over plain HTTP, and IP matching doesn't depend on the Host header. It has
/// no port syntax, so it re-opens every published host port (see
/// docs/design/agentgateway.md).
pub const GATEWAY_IP: &str = "192.168.127.254";

/// Built-in presets. Compiled in on purpose: nothing a box can write can
/// widen them, and a change goes through review and a release.
pub const PRESETS: &[(&str, &[&str])] = &[
    (
        "github",
        &[
            "github.com",
            "api.github.com",
            "codeload.github.com",
            "*.githubusercontent.com",
            "uploads.github.com",
        ],
    ),
    ("npm", &["registry.npmjs.org"]),
    ("crates", &["crates.io", "index.crates.io", "static.crates.io"]),
    ("pypi", &["pypi.org", "files.pythonhosted.org"]),
    ("debian", &["deb.debian.org", "security.debian.org"]),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Policy {
    /// No restriction (BoxLite's `Enabled` with an empty list). Today's default.
    Open,
    /// Only these rules, already expanded, lowercased, sorted and deduped,
    /// always including `GATEWAY_IP`.
    Allow(Vec<String>),
    /// No network interface at all.
    Disabled,
}

/// A box's policy as BoxLite recorded it at creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    Known(Policy),
    /// BoxLite returned no network metadata; never equal to a request.
    Unknown,
}

pub fn resolve(rules: &[String], disabled: bool) -> Result<Policy> {
    if disabled {
        if !rules.is_empty() {
            bail!("--network disabled cannot be combined with --allow-net");
        }
        return Ok(Policy::Disabled);
    }
    if rules.is_empty() {
        return Ok(Policy::Open);
    }
    let mut out = vec![GATEWAY_IP.to_string()];
    for rule in rules {
        if let Some(name) = rule.strip_prefix('@') {
            let Some((_, hosts)) = PRESETS.iter().find(|(n, _)| *n == name) else {
                bail!("--allow-net: unknown preset {rule:?}; valid presets: {}", preset_names());
            };
            out.extend(hosts.iter().map(|h| h.to_string()));
        } else {
            let rule = rule.to_ascii_lowercase();
            validate_rule(&rule)?;
            out.push(rule);
        }
    }
    Ok(Policy::Allow(normalize(out)))
}

fn preset_names() -> String {
    PRESETS.iter().map(|(n, _)| format!("@{n}")).collect::<Vec<_>>().join(", ")
}

fn normalize(mut v: Vec<String>) -> Vec<String> {
    for s in &mut v {
        *s = s.to_ascii_lowercase();
    }
    v.sort();
    v.dedup();
    v
}

/// Accepts what BoxLite accepts: an exact host, `*.domain`, an IP, or a CIDR.
/// Rejects the likely mistakes (URLs, ports, paths) instead of passing on a
/// rule that would silently match nothing.
fn validate_rule(rule: &str) -> Result<()> {
    const HINT: &str = "rules are a host, *.domain, an IP or a CIDR (no scheme, port or path)";
    if rule.is_empty() {
        bail!("--allow-net: empty rule");
    }
    if rule.chars().any(char::is_whitespace) {
        bail!("--allow-net: {rule:?} contains whitespace");
    }
    if let Some((addr, prefix)) = rule.split_once('/') {
        let ip: IpAddr = addr
            .parse()
            .map_err(|_| anyhow::anyhow!("--allow-net: {rule:?} is not a valid CIDR; {HINT}"))?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        match prefix.parse::<u8>() {
            Ok(p) if p <= max => return Ok(()),
            _ => bail!("--allow-net: {rule:?} has an invalid prefix length; {HINT}"),
        }
    }
    if rule.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    if rule.contains(':') {
        bail!("--allow-net: {rule:?} looks like a URL or host:port; {HINT}");
    }
    let host = match rule.strip_prefix("*.") {
        Some(rest) => rest,
        None => rule,
    };
    let valid_label = |l: &str| {
        !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if host.is_empty() || !host.split('.').all(valid_label) {
        bail!("--allow-net: {rule:?} is not a valid host or *.domain; {HINT}");
    }
    Ok(())
}

impl Policy {
    pub fn to_spec(&self) -> NetworkSpec {
        match self {
            Policy::Open => NetworkSpec::Enabled { allow_net: Vec::new() },
            Policy::Allow(list) => NetworkSpec::Enabled { allow_net: list.clone() },
            Policy::Disabled => NetworkSpec::Disabled,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Policy::Open => "open (unrestricted)".into(),
            Policy::Disabled => "disabled (no network)".into(),
            Policy::Allow(list) => list
                .iter()
                .map(|h| {
                    if h == GATEWAY_IP { format!("{h} (gateway, implicit)") } else { h.clone() }
                })
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

impl Recorded {
    pub fn from_info(info: Option<&NetworkInfo>) -> Recorded {
        let Some(info) = info else { return Recorded::Unknown };
        match info.outbound.mode {
            NetworkMode::Disabled => Recorded::Known(Policy::Disabled),
            NetworkMode::Enabled if info.outbound.allow_net.is_empty() => {
                Recorded::Known(Policy::Open)
            }
            NetworkMode::Enabled => {
                Recorded::Known(Policy::Allow(normalize(info.outbound.allow_net.clone())))
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Recorded::Known(p) => p.describe(),
            Recorded::Unknown => "unknown (not recorded by BoxLite)".into(),
        }
    }
}

pub fn secret_hosts(secrets: &[Secret]) -> Vec<String> {
    normalize(secrets.iter().flat_map(|s| s.hosts.iter().cloned()).collect())
}

pub fn policy_lines(policy: &Policy, secret_hosts: &[String], runs_claude: bool) -> Vec<String> {
    match policy {
        Policy::Open => Vec::new(),
        Policy::Allow(_) => {
            let mut lines = vec![
                format!("cbox: egress restricted to: {}", policy.describe()),
                "cbox: note: the gateway IP opens every published host port, not just \
                 :15002/:15003 (see docs/design/agentgateway.md)"
                    .to_string(),
            ];
            if !secret_hosts.is_empty() {
                lines.push(format!(
                    "cbox: also reachable on :443 via secrets: {}",
                    secret_hosts.join(", ")
                ));
            }
            lines
        }
        Policy::Disabled => {
            let mut lines = vec!["cbox: network disabled".to_string()];
            if runs_claude {
                lines.push(
                    "cbox: warning: claude cannot reach the Anthropic API or MCP through the \
                     gateway with the network disabled; pass -- <cmd> (e.g. -- bash) to run \
                     something else"
                        .to_string(),
                );
            }
            lines
        }
    }
}

/// Warning for the one way `--allow-net` silently breaks Claude: it runs in
/// the box, has no gateway `ANTHROPIC_BASE_URL`, and so talks to
/// api.anthropic.com directly, a host the allow-list doesn't cover.
pub fn direct_anthropic_warning(
    policy: &Policy,
    runs_claude: bool,
    base_url: Option<&str>,
) -> Option<String> {
    let Policy::Allow(list) = policy else { return None };
    if !runs_claude || base_url.is_some_and(|u| u.contains("host.boxlite.internal")) {
        return None;
    }
    if list.iter().any(|r| r == "api.anthropic.com" || r == "*.anthropic.com") {
        return None;
    }
    Some(
        "cbox: warning: claude talks to api.anthropic.com directly (no gateway \
         ANTHROPIC_BASE_URL), which this allow-list blocks; add --allow-net \
         api.anthropic.com or use the gateway"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxlite::{InboundNetworkInfo, OutboundNetworkInfo};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn info(mode: NetworkMode, allow: &[&str]) -> NetworkInfo {
        NetworkInfo::new(
            OutboundNetworkInfo { mode, allow_net: s(allow) },
            InboundNetworkInfo::default(),
            None,
        )
    }

    #[test]
    fn no_rules_is_open() {
        assert_eq!(resolve(&[], false).unwrap(), Policy::Open);
    }

    #[test]
    fn disabled_is_disabled_and_rejects_rules() {
        assert_eq!(resolve(&[], true).unwrap(), Policy::Disabled);
        let err = resolve(&s(&["github.com"]), true).unwrap_err().to_string();
        assert!(err.contains("--network disabled"), "{err}");
    }

    #[test]
    fn a_preset_expands_and_the_gateway_is_added() {
        let Policy::Allow(list) = resolve(&s(&["@npm"]), false).unwrap() else { panic!() };
        assert_eq!(list, s(&[GATEWAY_IP, "registry.npmjs.org"]));
    }

    #[test]
    fn every_preset_resolves() {
        for (name, hosts) in PRESETS {
            let Policy::Allow(list) = resolve(&[format!("@{name}")], false).unwrap() else {
                panic!("@{name}")
            };
            for h in *hosts {
                assert!(list.contains(&h.to_string()), "@{name} missing {h}");
            }
        }
    }

    #[test]
    fn an_unknown_preset_lists_the_valid_ones() {
        let err = resolve(&s(&["@nope"]), false).unwrap_err().to_string();
        assert!(err.contains("@nope"), "{err}");
        for (name, _) in PRESETS {
            assert!(err.contains(&format!("@{name}")), "{err}");
        }
    }

    #[test]
    fn rules_are_lowercased_and_deduped() {
        let Policy::Allow(list) =
            resolve(&s(&["GitHub.com", "github.com", "@github"]), false).unwrap()
        else {
            panic!()
        };
        assert_eq!(list.iter().filter(|h| *h == "github.com").count(), 1);
        assert!(list.iter().all(|h| *h == h.to_ascii_lowercase()));
        let mut sorted = list.clone();
        sorted.sort();
        assert_eq!(list, sorted);
    }

    #[test]
    fn ipv4_ipv6_and_cidrs_are_accepted() {
        for r in ["10.0.0.1", "10.0.0.0/8", "::1", "fd00::/8", "*.example.com", "a-b.example.com"] {
            assert!(resolve(&s(&[r]), false).is_ok(), "{r} should be accepted");
        }
    }

    #[test]
    fn urls_ports_and_paths_are_rejected() {
        for r in [
            "https://github.com",
            "github.com:443",
            "github.com/org",
            "10.0.0.0/33",
            "",
            "git hub.com",
            "a.*.com",
            "*github.com",
            "*",
            "bad_host!.com",
        ] {
            assert!(resolve(&s(&[r]), false).is_err(), "{r:?} should be rejected");
        }
    }

    #[test]
    fn to_spec_maps_each_policy() {
        assert!(matches!(
            Policy::Open.to_spec(),
            NetworkSpec::Enabled { allow_net } if allow_net.is_empty()
        ));
        assert!(matches!(Policy::Disabled.to_spec(), NetworkSpec::Disabled));
        let p = resolve(&s(&["@npm"]), false).unwrap();
        assert!(matches!(
            p.to_spec(),
            NetworkSpec::Enabled { allow_net } if allow_net == s(&[GATEWAY_IP, "registry.npmjs.org"])
        ));
    }

    #[test]
    fn recorded_round_trips_what_to_spec_produced() {
        assert_eq!(Recorded::from_info(None), Recorded::Unknown);
        assert_eq!(
            Recorded::from_info(Some(&info(NetworkMode::Enabled, &[]))),
            Recorded::Known(Policy::Open)
        );
        assert_eq!(
            Recorded::from_info(Some(&info(NetworkMode::Disabled, &[]))),
            Recorded::Known(Policy::Disabled)
        );
        let p = resolve(&s(&["@github", "10.0.0.0/8"]), false).unwrap();
        let Policy::Allow(list) = &p else { panic!() };
        // Recorded order differs from ours: equality must not depend on it.
        let mut reversed: Vec<&str> = list.iter().map(String::as_str).collect();
        reversed.reverse();
        assert_eq!(
            Recorded::from_info(Some(&info(NetworkMode::Enabled, &reversed))),
            Recorded::Known(p)
        );
    }

    #[test]
    fn describe_marks_the_gateway() {
        let d = resolve(&s(&["@npm"]), false).unwrap().describe();
        assert!(d.contains("192.168.127.254 (gateway, implicit)"), "{d}");
        assert!(d.contains("registry.npmjs.org"), "{d}");
        assert!(Policy::Open.describe().contains("open"));
        assert!(Policy::Disabled.describe().contains("disabled"));
        assert!(Recorded::Unknown.describe().contains("unknown"));
    }

    #[test]
    fn secret_hosts_are_collected_sorted_and_deduped() {
        let sec = |hosts: &[&str]| Secret {
            name: "x".into(),
            hosts: s(hosts),
            placeholder: "<p>".into(),
            value: "v".into(),
        };
        let got = secret_hosts(&[sec(&["github.com"]), sec(&["api.github.com", "github.com"])]);
        assert_eq!(got, s(&["api.github.com", "github.com"]));
    }

    #[test]
    fn policy_lines_for_each_policy() {
        assert!(policy_lines(&Policy::Open, &s(&["github.com"]), true).is_empty());

        let allow = resolve(&s(&["@npm"]), false).unwrap();
        let lines = policy_lines(&allow, &[], true);
        assert!(lines[0].starts_with("cbox: egress restricted to: "), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("every published host port")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("via secrets")), "{lines:?}");

        let lines = policy_lines(&allow, &s(&["github.com"]), true);
        assert!(
            lines.iter().any(|l| l == "cbox: also reachable on :443 via secrets: github.com"),
            "{lines:?}"
        );

        let lines = policy_lines(&Policy::Disabled, &[], true);
        assert_eq!(lines[0], "cbox: network disabled");
        assert!(lines.iter().any(|l| l.contains("warning") && l.contains("claude")), "{lines:?}");
        assert_eq!(policy_lines(&Policy::Disabled, &[], false), s(&["cbox: network disabled"]));
    }

    #[test]
    fn direct_anthropic_warning_only_when_claude_goes_direct_under_an_allow_list() {
        let allow = resolve(&s(&["@npm"]), false).unwrap();
        let gw = Some("http://host.boxlite.internal:15002/api");
        assert!(direct_anthropic_warning(&allow, true, None).unwrap().contains("api.anthropic.com"));
        assert!(direct_anthropic_warning(&allow, true, Some("https://proxy.example")).is_some());
        assert!(direct_anthropic_warning(&allow, true, gw).is_none());
        assert!(direct_anthropic_warning(&allow, false, None).is_none());
        assert!(direct_anthropic_warning(&Policy::Open, true, None).is_none());
        assert!(direct_anthropic_warning(&Policy::Disabled, true, None).is_none());
        for rule in ["api.anthropic.com", "*.anthropic.com"] {
            let ok = resolve(&s(&[rule]), false).unwrap();
            assert!(direct_anthropic_warning(&ok, true, None).is_none(), "{rule}");
        }
    }
}
