//! Windows Credential Manager from WSL, through `powershell.exe -EncodedCommand`.
//!
//! The script (`wincred.ps1`) is compiled into the binary and handed to PowerShell
//! as base64 on the command line, so no script file ever exists for anyone to edit.
//! The secret travels over stdin only: never argv (visible in `ps`), never an error.
//! The script and flags were verified on Windows PowerShell 5.1 by a spike
//! (spikes/encodedcommand on the scratch branch); this Rust module has not been run on WSL.
//! Exit code 3 means "no such entry" (locale-proof); any other non-zero exit is an error.

use super::store::SecretStore;
use super::{StoredSecret, WSL_MAX_ENTRY_BYTES, check_size};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::io::Write;
use std::process::{Command, Stdio};

const SCRIPT: &str = include_str!("wincred.ps1");
const LABEL: &str = "Windows Credential Manager";

pub fn is_wsl_release(osrelease: &str) -> bool {
    osrelease.to_ascii_lowercase().contains("microsoft")
}

pub fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease").is_ok_and(|s| is_wsl_release(&s))
}

/// Base64 of the UTF-16LE script with `$Op` and `$Target` assigned first. The target is
/// inlined, which is safe only because the charset is checked here.
fn encode_command(op: &str, target: &str) -> Result<String> {
    if !matches!(op, "set" | "get" | "del") {
        bail!("internal error: unknown credential operation {op:?}");
    }
    let target_ok = target.len() > "cbox/".len()
        && target.starts_with("cbox/")
        && target.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'/'));
    if !target_ok {
        bail!("internal error: credential target {target:?} is outside the allowed charset");
    }
    let script = format!("$Op='{op}'; $Target='{target}'\n{SCRIPT}");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    Ok(BASE64.encode(utf16))
}

fn spawn_error(e: &std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!(
            "powershell.exe was not found: cbox needs it to reach Windows Credential Manager from WSL. \
             Check that WSL interop is enabled (`interop` in /etc/wsl.conf, and /proc/sys/fs/binfmt_misc/WSLInterop exists) \
             and that /mnt/c/Windows/System32/WindowsPowerShell/v1.0 is on PATH"
        )
    } else {
        anyhow!("cannot run powershell.exe: {e}")
    }
}

const NOT_FOUND_EXIT: i32 = 3;

struct Outcome {
    stdout: String,
    code: Option<i32>,
    /// PowerShell's stderr, CR-stripped and truncated (never contains the secret).
    stderr: String,
}

impl Outcome {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
    fn not_found(&self) -> bool {
        self.code == Some(NOT_FOUND_EXIT)
    }
}

fn run(op: &str, name: &str, stdin: Option<&str>) -> Result<Outcome> {
    let cmd = encode_command(op, &format!("cbox/{name}"))?;
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_error(&e))?;
    let mut pipe = child.stdin.take().expect("stdin was piped");
    if let Some(data) = stdin {
        // A write error means PowerShell already exited; its exit status explains why.
        let _ = pipe.write_all(data.as_bytes());
    }
    drop(pipe);
    let out = child.wait_with_output().context("waiting for powershell.exe")?;
    let err = String::from_utf8_lossy(&out.stderr).replace('\r', "");
    Ok(Outcome {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        code: out.status.code(),
        stderr: err.trim().chars().take(300).collect(),
    })
}

pub struct WinCredStore;

impl SecretStore for WinCredStore {
    fn label(&self) -> &'static str {
        LABEL
    }
    fn max_entry_bytes(&self) -> Option<usize> {
        Some(WSL_MAX_ENTRY_BYTES)
    }
    fn get(&self, name: &str) -> Result<Option<StoredSecret>> {
        let out = run("get", name, None)?;
        if out.not_found() {
            return Ok(None);
        }
        if !out.ok() {
            bail!("reading secret {name:?} from {LABEL}: {}", out.stderr);
        }
        Ok(Some(StoredSecret::from_json(&out.stdout).with_context(|| format!("secret {name:?} in {LABEL}"))?))
    }
    fn exists(&self, name: &str) -> Result<bool> {
        let out = run("get", name, None)?;
        if out.not_found() {
            return Ok(false);
        }
        if !out.ok() {
            bail!("reading secret {name:?} from {LABEL}: {}", out.stderr);
        }
        Ok(true)
    }
    fn put(&self, name: &str, s: &StoredSecret) -> Result<()> {
        let json = s.to_json();
        check_size(&json, self.max_entry_bytes())?;
        let out = run("set", name, Some(&json))?;
        if !out.ok() {
            bail!("writing secret {name:?} to {LABEL}: {}", out.stderr);
        }
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<bool> {
        let out = run("del", name, None)?;
        if out.not_found() {
            return Ok(false);
        }
        if !out.ok() {
            bail!("deleting secret {name:?} from {LABEL}: {}", out.stderr);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(b64: &str) -> String {
        let bytes = BASE64.decode(b64).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn encode_command_is_base64_utf16le_and_inlines_op_and_target() {
        let script = decode(&encode_command("get", "cbox/openai").unwrap());
        assert!(script.starts_with("$Op='get'; $Target='cbox/openai'\n"));
        assert!(script.ends_with(SCRIPT));
    }

    #[test]
    fn encode_command_rejects_a_target_outside_the_name_charset() {
        for bad in ["cbox/a'; calc; '", "cbox/a b", "cbox/A", "other/a", "cbox/a\n", "cbox/é", "cbox/"] {
            assert!(encode_command("get", bad).is_err(), "{bad:?}");
        }
        assert!(encode_command("rm", "cbox/a").is_err());
    }

    #[test]
    fn is_wsl_matches_the_microsoft_kernel_string() {
        assert!(is_wsl_release("5.15.167.4-microsoft-standard-WSL2\n"));
        assert!(is_wsl_release("4.4.0-19041-Microsoft"));
        assert!(!is_wsl_release("6.12.87\n"));
    }

    #[test]
    fn a_missing_powershell_error_names_the_missing_piece() {
        let msg = spawn_error(&std::io::Error::from(std::io::ErrorKind::NotFound)).to_string();
        assert!(msg.contains("powershell.exe was not found") && msg.contains("interop"));
        assert!(spawn_error(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)).to_string().contains("cannot run powershell.exe"));
    }

    #[test]
    fn an_entry_over_2560_bytes_is_rejected_before_spawning() {
        let big = StoredSecret { value: "a".repeat(2600), hosts: vec!["api.example.com".into()], env: "K".into() };
        let err = WinCredStore.put("big", &big).unwrap_err().to_string();
        assert!(err.contains("over the 2560-byte limit"), "{err}");
    }

    /// Runs `f` with a fake powershell.exe first on PATH. The fake records stdin and argv
    /// in its directory and behaves per `scenario`: ok:<stdout> | nf | fail:<stderr>.
    #[cfg(unix)]
    fn with_fake<T>(scenario: &str, f: impl FnOnce(&std::path::Path) -> T) -> T {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::test_env_lock::lock();
        let dir = std::env::temp_dir().join(format!("cbox-fakeps-{}-{}", std::process::id(), scenario.len() + scenario.bytes().map(usize::from).sum::<usize>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scenario"), scenario).unwrap();
        let sh = dir.join("powershell.exe");
        std::fs::write(
            &sh,
            "#!/bin/sh\nd=$(dirname \"$0\")\ncat > \"$d/stdin\"\nprintf '%s\\n' \"$@\" > \"$d/args\"\n\
             s=$(cat \"$d/scenario\")\ncase \"$s\" in\n ok:*) printf '%s' \"${s#ok:}\"; exit 0;;\n nf) exit 3;;\n fail:*) printf '%s\\r\\n' \"${s#fail:}\" >&2; exit 1;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _rm = RemoveDir(dir.clone());
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        let _path = crate::test_env_lock::EnvVarGuard::set("PATH", &path);
        f(&dir)
    }

    struct RemoveDir(std::path::PathBuf);
    impl Drop for RemoveDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_panicking_closure_still_restores_path_and_removes_the_dir() {
        let before = std::env::var("PATH").ok();
        let mut seen = None;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| with_fake("nf", |d| { seen = Some(d.to_path_buf()); panic!("boom") })));
        assert!(r.is_err());
        let _lock = crate::test_env_lock::lock();
        assert_eq!(std::env::var("PATH").ok(), before);
        assert!(!seen.unwrap().exists());
    }

    const JSON: &str = r#"{"value":"sk-abc","hosts":["a.com"],"env":"K"}"#;

    #[test]
    fn get_maps_exit_codes_to_some_none_and_err() {
        let some = with_fake(&format!("ok:{JSON}"), |_| WinCredStore.get("x"));
        assert_eq!(some.unwrap().unwrap().value, "sk-abc");
        assert!(with_fake("nf", |_| WinCredStore.get("x")).unwrap().is_none());
        let e = with_fake("fail:boom", |_| WinCredStore.get("x")).unwrap_err().to_string();
        assert!(e.contains("reading secret \"x\" from Windows Credential Manager: boom"), "{e}");
        assert!(with_fake("ok:", |_| WinCredStore.get("x")).is_err(), "empty stdout is not valid JSON");
    }

    #[test]
    fn exists_does_not_parse_and_maps_exit_codes() {
        assert!(with_fake("ok:not json", |_| WinCredStore.exists("x")).unwrap());
        assert!(!with_fake("nf", |_| WinCredStore.exists("x")).unwrap());
        assert!(with_fake("fail:boom", |_| WinCredStore.exists("x")).is_err());
    }

    #[test]
    fn delete_maps_exit_codes() {
        assert!(with_fake("ok:", |_| WinCredStore.delete("x")).unwrap());
        assert!(!with_fake("nf", |_| WinCredStore.delete("x")).unwrap());
        assert!(with_fake("fail:boom", |_| WinCredStore.delete("x")).is_err());
    }

    #[test]
    fn put_sends_the_json_on_stdin_only() {
        let s = StoredSecret { value: "sk-abc".into(), hosts: vec!["a.com".into()], env: "K".into() };
        let (stdin, args) = with_fake("ok:", |d| {
            WinCredStore.put("x", &s).unwrap();
            (std::fs::read_to_string(d.join("stdin")).unwrap(), std::fs::read_to_string(d.join("args")).unwrap())
        });
        assert_eq!(stdin, s.to_json());
        let lines: Vec<&str> = args.lines().collect();
        assert_eq!(lines[..3], ["-NoProfile", "-NonInteractive", "-EncodedCommand"]);
        assert!(!args.contains("sk-abc") && !decode(lines[3]).contains("sk-abc"));
        assert!(decode(lines[3]).starts_with("$Op='set'; $Target='cbox/x'\n"));
        let e = with_fake("fail:denied", |_| WinCredStore.put("x", &s)).unwrap_err().to_string();
        assert!(e.contains("denied") && !e.contains("sk-abc"), "{e}");
    }

    #[test]
    fn stderr_is_cr_stripped_and_truncated() {
        let e = with_fake(&format!("fail:{}", "e".repeat(400)), |_| WinCredStore.get("x")).unwrap_err().to_string();
        assert!(!e.contains('\r') && e.matches('e').count() < 400 + 40 && !e.contains(&"e".repeat(301)));
    }

    #[test]
    fn run_reports_a_missing_powershell() {
        let _lock = crate::test_env_lock::lock();
        let _path = crate::test_env_lock::EnvVarGuard::set("PATH", "/nonexistent-cbox-dir");
        let r = WinCredStore.get("x");
        assert!(r.unwrap_err().to_string().contains("powershell.exe was not found"));
    }
}
