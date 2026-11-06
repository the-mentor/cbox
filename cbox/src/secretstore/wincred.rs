//! Windows Credential Manager from WSL, through `powershell.exe -EncodedCommand`.
//!
//! The script (`wincred.ps1`) is compiled into the binary and handed to PowerShell
//! as base64 on the command line, so no script file ever exists for anyone to edit.
//! The secret travels over stdin only: never argv (visible in `ps`), never an error.
//! Verified on Windows PowerShell 5.1 under WSL: stdin, UTF-8, "Element not found",
//! exit code 1, a 27 KB command line (Windows caps it at 32767; ours is about 6 KB).

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
    let target_ok = target.starts_with("cbox/")
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

struct Outcome {
    stdout: String,
    /// `None` on success, otherwise PowerShell's stderr (never contains the secret).
    failure: Option<String>,
}

impl Outcome {
    fn not_found(&self) -> bool {
        self.failure.as_deref().is_some_and(|e| e.contains("Element not found"))
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
    let failure = (!out.status.success()).then(|| {
        let err = String::from_utf8_lossy(&out.stderr).replace('\r', "");
        err.trim().chars().take(300).collect()
    });
    Ok(Outcome { stdout: String::from_utf8_lossy(&out.stdout).into_owned(), failure })
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
        if let Some(e) = out.failure {
            bail!("reading secret {name:?} from {LABEL}: {e}");
        }
        Ok(Some(StoredSecret::from_json(&out.stdout).with_context(|| format!("secret {name:?} in {LABEL}"))?))
    }
    fn exists(&self, name: &str) -> Result<bool> {
        let out = run("get", name, None)?;
        match out.failure {
            None => Ok(true),
            Some(_) if out.not_found() => Ok(false),
            Some(e) => bail!("reading secret {name:?} from {LABEL}: {e}"),
        }
    }
    fn put(&self, name: &str, s: &StoredSecret) -> Result<()> {
        let json = s.to_json();
        check_size(&json, self.max_entry_bytes())?;
        match run("set", name, Some(&json))?.failure {
            None => Ok(()),
            Some(e) => bail!("writing secret {name:?} to {LABEL}: {e}"),
        }
    }
    fn delete(&self, name: &str) -> Result<bool> {
        let out = run("del", name, None)?;
        match out.failure {
            None => Ok(true),
            Some(_) if out.not_found() => Ok(false),
            Some(e) => bail!("deleting secret {name:?} from {LABEL}: {e}"),
        }
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
        for bad in ["cbox/a'; calc; '", "cbox/a b", "cbox/A", "other/a", "cbox/a\n", "cbox/é"] {
            assert!(encode_command("get", bad).is_err(), "{bad:?}");
        }
        assert!(encode_command("rm", "cbox/a").is_err());
    }

    #[test]
    fn script_reads_stdin_and_never_contains_a_value() {
        assert!(SCRIPT.contains("[Console]::In.ReadToEnd()"));
        assert!(!SCRIPT.contains("param("), "op and target are prepended, not parameters");
        // The command line carries only the script plus op and target.
        let all = decode(&encode_command("set", "cbox/x").unwrap());
        assert_eq!(all.len(), "$Op='set'; $Target='cbox/x'\n".len() + SCRIPT.len());
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
}
