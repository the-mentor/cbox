pub mod down;
pub mod exec;
pub mod list;
pub mod profile;
pub mod secret;
pub mod up;

/// The in-box command when the user gives no `-- <cmd>`: the image's `sandbox-agent`
/// launcher, or `claude` in an image built before it existed (an old `cbox-custom`, or a
/// box created from one), so upgrading cbox does not break existing boxes.
pub fn default_agent_cmd(args: &[&str]) -> Vec<String> {
    let script = r#"command -v sandbox-agent >/dev/null && exec sandbox-agent "$@"; exec claude "$@""#;
    ["sh", "-c", script, "sh"].iter().chain(args).map(|s| s.to_string()).collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::default_agent_cmd;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    /// Runs the default command with only the given stub executables on PATH; each
    /// stub prints its name and arguments.
    fn run(stubs: &[&str], args: &[&str]) -> String {
        let dir = std::env::temp_dir().join(format!("cbox-agent-cmd-{}-{}", std::process::id(), stubs.join("-")));
        std::fs::create_dir_all(&dir).unwrap();
        for s in stubs {
            let p = dir.join(s);
            std::fs::write(&p, format!("#!/bin/sh\necho {s} \"$@\"\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cmd = default_agent_cmd(args);
        let out = Command::new(&cmd[0])
            .args(&cmd[1..])
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn prefers_sandbox_agent_and_falls_back_to_claude() {
        assert_eq!(run(&["sandbox-agent", "claude"], &["--continue"]), "sandbox-agent --continue\n");
        assert_eq!(run(&["claude"], &["--continue"]), "claude --continue\n");
        assert_eq!(run(&["claude"], &[]), "claude\n");
    }
}
