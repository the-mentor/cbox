//! The command a box runs when the user names none (`cbox up` with no `-- <cmd>`,
//! `cbox exec` with no `-- <cmd>`).
//!
//! Agent images ship `sandbox-agent`, a launcher that starts whichever agent the
//! image has (claude, codex or opencode); `sandbox-agent --continue` resumes its
//! last session. Images from before it existed (`cbox-custom` built on `cbox-base`,
//! and boxes already created from it) only have `claude`, so those fall back to it.
//!
//! The launcher runs under `sh -c` on purpose. Started directly, an interactive
//! agent returned immediately in a box (seen with claude and codex, no output, exit
//! at once), while the same command behind a shell stayed open. The cause was not
//! found; the shell also carries the fallback above.

const LAUNCH: &str = r#"if command -v sandbox-agent >/dev/null 2>&1; then exec sandbox-agent "$@"; else exec claude "$@"; fi"#;

fn launch(args: &[&str]) -> Vec<String> {
    let mut cmd = vec!["sh".to_string(), "-c".to_string(), LAUNCH.to_string(), "sandbox-agent".to_string()];
    cmd.extend(args.iter().map(|a| (*a).to_string()));
    cmd
}

/// What `cbox up` runs with no command: start the image's agent.
pub fn default_up() -> Vec<String> {
    launch(&[])
}

/// What `cbox exec` runs with no command: the image's agent, on its last session.
pub fn default_exec() -> Vec<String> {
    launch(&["--continue"])
}

/// Whether the default command may end up running claude in this image, which is
/// when the missing-Anthropic-credential warning applies. Only the image name is
/// known here, so an image named for another agent is assumed not to.
pub fn default_may_be_claude(image: &str) -> bool {
    let image = image.to_ascii_lowercase();
    !image.contains("codex") && !image.contains("opencode")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    /// Runs `cmd` with a PATH that holds only `stubs` (name -> what it prints) plus
    /// the system dirs, so the result shows which launcher the shell snippet picked.
    fn run_with_stubs(cmd: &[String], stubs: &[&str]) -> String {
        let dir = std::env::temp_dir().join(format!("cbox-agentcmd-{}-{}", std::process::id(), stubs.join("-")));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in stubs {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho \"{name} $*\"\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = Command::new(&cmd[0])
            .args(&cmd[1..])
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn up_and_exec_defaults_differ_only_by_continue() {
        assert_eq!(default_up()[..4], default_exec()[..4]);
        assert_eq!(default_up().len(), 4);
        assert_eq!(default_exec().last().map(String::as_str), Some("--continue"));
    }

    #[test]
    fn runs_the_launcher_when_the_image_has_one() {
        assert_eq!(run_with_stubs(&default_up(), &["sandbox-agent", "claude"]), "sandbox-agent");
        assert_eq!(run_with_stubs(&default_exec(), &["sandbox-agent"]), "sandbox-agent --continue");
    }

    #[test]
    fn falls_back_to_claude_for_images_without_the_launcher() {
        assert_eq!(run_with_stubs(&default_up(), &["claude"]), "claude");
        assert_eq!(run_with_stubs(&default_exec(), &["claude"]), "claude --continue");
    }

    #[test]
    fn the_credential_warning_skips_images_named_for_another_agent() {
        assert!(default_may_be_claude("cbox-custom"));
        assert!(default_may_be_claude("ghcr.io/the-mentor/cbox-agent-claude:latest"));
        assert!(!default_may_be_claude("localhost:5551/library/cbox-agent-codex"));
        assert!(!default_may_be_claude("ghcr.io/the-mentor/cbox-agent-OpenCode:pr-160"));
    }
}
