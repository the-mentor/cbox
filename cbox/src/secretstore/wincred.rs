//! Windows Credential Manager from WSL, through `powershell.exe -EncodedCommand`.
//!
//! The script (`wincred.ps1`) is compiled into the binary and handed to PowerShell
//! as base64 on the command line, so no script file ever exists for anyone to edit.
//! The secret travels over stdin only: never argv (visible in `ps`), never an error.
//! The script and flags were checked on real Windows PowerShell 5.1 with a standalone script
//! (19 checks, including `getmany` and the `!` marker), and this
//! module was round-tripped by hand on WSL (create, duplicate, list, edit, not-found, size
//! limit, `up --secret`, and batched `list` and `up` with one and with three secrets).
//! Not verified: a non-English locale, interop off, PowerShell blocked by policy or
//! Constrained Language Mode, a hung PowerShell (there is no timeout), a macOS build.
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

/// Any one signal is enough: custom WSL kernels need not say "microsoft".
pub fn is_wsl_from(osrelease: &str, distro_env: Option<&str>, interop_file_exists: bool) -> bool {
    is_wsl_release(osrelease) || distro_env.is_some_and(|d| !d.is_empty()) || interop_file_exists
}

pub fn is_wsl() -> bool {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    is_wsl_from(&release, std::env::var("WSL_DISTRO_NAME").ok().as_deref(), std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists())
}

/// Base64 of the UTF-16LE script with `$Op` and `$Target` assigned first. The target is
/// inlined, which is safe only because the charset is checked here.
fn encode_command(op: &str, target: &str) -> Result<String> {
    if !matches!(op, "set" | "get" | "del" | "getmany") {
        bail!("internal error: unknown credential operation {op:?}");
    }
    if op == "getmany" {
        // Names travel over stdin, so there is no target to inline.
        if !target.is_empty() {
            bail!("internal error: getmany takes no target");
        }
        return Ok(encode_script(&format!("$Op='getmany'; $Target=''\n{SCRIPT}")));
    }
    let target_ok = target.len() > "cbox/".len()
        && target.starts_with("cbox/")
        && target.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'/'));
    if !target_ok {
        bail!("internal error: credential target {target:?} is outside the allowed charset");
    }
    Ok(encode_script(&format!("$Op='{op}'; $Target='{target}'\n{SCRIPT}")))
}

fn encode_script(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    BASE64.encode(utf16)
}

fn spawn_error(e: &std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!(
            "powershell.exe was not found: cbox needs it to reach Windows Credential Manager from WSL. \
             Check that WSL interop is enabled (`interop` in /etc/wsl.conf, and /proc/sys/fs/binfmt_misc/WSLInterop exists) \
             and that /mnt/c/Windows/System32/WindowsPowerShell/v1.0 is on PATH"
        )
    } else {
        anyhow!("cannot run powershell.exe: {e} (cbox reaches Windows Credential Manager through WSL interop and powershell.exe on PATH)")
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

const PROGRAM: &str = "powershell.exe";

fn run(op: &str, name: &str, stdin: Option<&str>) -> Result<Outcome> {
    run_with(PROGRAM, op, name, stdin)
}

fn run_with(program: &str, op: &str, name: &str, stdin: Option<&str>) -> Result<Outcome> {
    exec(program, &encode_command(op, &format!("cbox/{name}"))?, stdin)
}

fn exec(program: &str, cmd: &str, stdin: Option<&str>) -> Result<Outcome> {
    let mut child = Command::new(program)
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", cmd])
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
    fn get_many(&self, names: &[&str]) -> Vec<Result<Option<StoredSecret>>> {
        // Same charset the script re-checks; an invalid name is never sent.
        let valid = super::name_charset_ok;
        let sent: Vec<&str> = names.iter().copied().filter(|n| valid(n)).collect();
        let mut fetched: Result<std::collections::HashMap<&str, String>, String> = Ok(Default::default());
        if !sent.is_empty() {
            let stdin = sent.iter().map(|n| format!("{n}\n")).collect::<String>();
            fetched = match exec(PROGRAM, &encode_command("getmany", "").expect("getmany takes no target"), Some(&stdin)) {
                Err(e) => Err(format!("{e:#}")),
                Ok(out) if !out.ok() => Err(format!("reading secrets from {LABEL}: {}", out.stderr)),
                Ok(out) => Ok(out
                    .stdout
                    .lines()
                    .filter_map(|l| l.trim_end_matches('\r').split_once('\t'))
                    .filter_map(|(n, v)| sent.iter().find(|s| **s == n).map(|s| (*s, v.to_string())))
                    .collect()),
            };
        }
        names
            .iter()
            .map(|&name| {
                if !valid(name) {
                    bail!("invalid secret name {name:?}");
                }
                let map = fetched.as_ref().map_err(|e| anyhow!("{e}"))?;
                let Some(entry) = map.get(name) else {
                    bail!("reading secret {name:?} from {LABEL}: no result returned");
                };
                match entry.as_str() {
                    "-" => return Ok(None),
                    "!" => {
                        // Error path only: one single get to learn the cause; its value is never used.
                        return Err(self.get(name).err().unwrap_or_else(|| anyhow!("reading secret {name:?} from {LABEL} failed")));
                    }
                    _ => {}
                }
                let b64 = entry;
                let json = BASE64.decode(b64).ok().and_then(|b| String::from_utf8(b).ok()).ok_or_else(|| anyhow!("reading secret {name:?} from {LABEL}: malformed result"))?;
                Ok(Some(StoredSecret::from_json(&json).with_context(|| format!("secret {name:?} in {LABEL}"))?))
            })
            .collect()
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
    fn is_wsl_from_accepts_any_of_three_signals() {
        assert!(is_wsl_from("6.6.0-custom\n", Some("Ubuntu"), false));
        assert!(is_wsl_from("6.6.0-custom\n", None, true));
        assert!(is_wsl_from("5.15-microsoft-standard-WSL2", None, false));
        assert!(!is_wsl_from("6.12.87\n", None, false));
        assert!(!is_wsl_from("6.12.87\n", Some(""), false), "an empty WSL_DISTRO_NAME is not a signal");
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
            "#!/bin/sh\nd=$(dirname \"$0\")\necho x >> \"$d/count\"\ncat > \"$d/stdin\"\nprintf '%s\\n' \"$@\" > \"$d/args\"\n\
             n=$(wc -l < \"$d/count\")\ns=$(cat \"$d/scenario.$n\" 2>/dev/null || cat \"$d/scenario\")\ncase \"$s\" in\n ok:*) printf '%s' \"${s#ok:}\"; exit 0;;\n nf) exit 3;; bad:*) printf '%s' \"${s#bad:}\"; echo oops >&2; exit 1;;\n fail:*) printf '%s\\r\\n' \"${s#fail:}\" >&2; exit 1;;\nesac\n",
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
        let mut seen = None;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| with_fake("nf", |d| { seen = Some(d.to_path_buf()); panic!("boom") })));
        assert!(r.is_err());
        let _lock = crate::test_env_lock::lock();
        assert!(!std::env::var("PATH").unwrap_or_default().contains("cbox-fakeps"));
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
        // Calls run_with directly: no PATH change, so parallel tests are unaffected.
        let r = run_with("cbox-no-such-powershell.exe", "get", "x", None);
        assert!(r.err().expect("must fail").to_string().contains("powershell.exe was not found"));
    }

    fn entry_line(name: &str, json: &str) -> String {
        format!("{name}\t{}\n", BASE64.encode(json))
    }

    fn calls(d: &std::path::Path) -> usize {
        std::fs::read_to_string(d.join("count")).unwrap().lines().count()
    }

    #[test]
    fn get_many_is_one_spawn_and_keeps_order_and_states() {
        let corrupt = r#"{"value":1}"#;
        let scenario = format!("ok:{}b\t-\n{}", entry_line("a", JSON), entry_line("c", corrupt));
        let (res, n, stdin, args) = with_fake(&scenario, |d| {
            let r = WinCredStore.get_many(&["a", "b", "c"]);
            (r, calls(d), std::fs::read_to_string(d.join("stdin")).unwrap(), std::fs::read_to_string(d.join("args")).unwrap())
        });
        assert_eq!(n, 1);
        assert_eq!(stdin, "a\nb\nc\n");
        let lines: Vec<&str> = args.lines().collect();
        assert!(!args.contains("a\nb") && lines.len() == 4);
        let script = decode(lines[3]);
        assert!(script.starts_with("$Op='getmany'; $Target=''\n"));
        assert_eq!(res.len(), 3);
        assert_eq!(res[0].as_ref().unwrap().as_ref().unwrap().value, "sk-abc");
        assert!(res[1].as_ref().unwrap().is_none());
        let e = format!("{:#}", res[2].as_ref().unwrap_err());
        assert!(e.contains("secret \"c\" in Windows Credential Manager"), "{e}");
    }

    #[test]
    fn get_many_does_not_send_an_invalid_name() {
        let scenario = format!("ok:{}", entry_line("a", JSON));
        let (res, stdin) = with_fake(&scenario, |d| (WinCredStore.get_many(&["a", "Bad'; x", "a/b"]), std::fs::read_to_string(d.join("stdin")).unwrap()));
        assert_eq!(stdin, "a\n");
        assert!(res[0].is_ok() && res[1].is_err() && res[2].is_err());
    }

    #[test]
    fn get_many_with_no_names_spawns_nothing() {
        let n = with_fake("ok:", |d| {
            assert!(WinCredStore.get_many(&[]).is_empty());
            d.join("count").exists()
        });
        assert!(!n);
    }

    #[test]
    fn get_many_whole_call_failure_is_an_error_for_every_entry() {
        let res = with_fake("fail:boom", |_| WinCredStore.get_many(&["a", "b"]));
        assert_eq!(res.len(), 2);
        for r in res {
            assert!(r.unwrap_err().to_string().contains("boom"));
        }
    }

    #[test]
    fn get_many_missing_output_line_is_an_error_not_none() {
        let scenario = format!("ok:{}", entry_line("a", JSON));
        let res = with_fake(&scenario, |_| WinCredStore.get_many(&["a", "b"]));
        assert!(res[0].as_ref().unwrap().is_some());
        assert!(res[1].as_ref().unwrap_err().to_string().contains("no result"));
    }

    #[test]
    fn the_getmany_script_text_contains_no_name() {
        let script = decode(&encode_command("getmany", "").unwrap());
        assert!(script.contains("-cnotmatch '^[a-z][a-z0-9_-]*$'"));
        assert!(encode_command("getmany", "cbox/x").is_err());
    }

    #[test]
    fn get_many_isolates_a_failed_entry_and_keeps_order() {
        let corrupt = r#"{"value":1}"#;
        let scenario = format!("ok:{}b\t!\nc\t-\n{}", entry_line("a", JSON), entry_line("d", corrupt));
        let res = with_fake(&scenario, |d| {
            std::fs::write(d.join("scenario.2"), "fail:denied").unwrap();
            WinCredStore.get_many(&["a", "b", "c", "d"])
        });
        assert_eq!(res[0].as_ref().unwrap().as_ref().unwrap().value, "sk-abc");
        let e = res[1].as_ref().unwrap_err().to_string();
        assert_eq!(e, "reading secret \"b\" from Windows Credential Manager: denied");
        assert!(res[2].as_ref().unwrap().is_none());
        assert!(res[3].is_err());
    }

    #[test]
    fn a_failed_entry_gets_one_follow_up_get_and_its_stderr() {
        let scenario = format!("ok:{}b\t!\n", entry_line("a", JSON));
        let (res, n) = with_fake(&scenario, |d| {
            std::fs::write(d.join("scenario.2"), "fail:access denied").unwrap();
            (WinCredStore.get_many(&["a", "b"]), calls(d))
        });
        assert_eq!(n, 2);
        assert!(res[0].as_ref().unwrap().is_some());
        let e = res[1].as_ref().unwrap_err().to_string();
        assert!(e.contains("access denied") && !e.contains("sk-abc"), "{e}");
        // A follow-up that unexpectedly succeeds keeps the plain error and never returns its value.
        let scenario = "ok:b\t!\n".to_string();
        let res = with_fake(&scenario, |d| {
            std::fs::write(d.join("scenario.2"), format!("ok:{JSON}")).unwrap();
            WinCredStore.get_many(&["b"])
        });
        assert_eq!(res[0].as_ref().unwrap_err().to_string(), "reading secret \"b\" from Windows Credential Manager failed");
    }

    #[test]
    fn get_many_tolerates_duplicates_crlf_blank_and_extra_lines() {
        let scenario = format!("ok:{}{}zz\t-\n\n", entry_line("a", JSON), entry_line("a", JSON)).replace('\n', "\r\n");
        let res = with_fake(&scenario, |_| WinCredStore.get_many(&["a", "a"]));
        assert!(res.iter().all(|r| r.as_ref().unwrap().is_some()));
    }

    #[test]
    fn get_many_never_sends_a_name_with_a_tab() {
        let scenario = format!("ok:{}", entry_line("a", JSON));
        let (res, stdin) = with_fake(&scenario, |d| (WinCredStore.get_many(&["a", "b\tc"]), std::fs::read_to_string(d.join("stdin")).unwrap()));
        assert_eq!(stdin, "a\n");
        assert!(res[0].is_ok() && res[1].is_err());
    }

    #[test]
    fn a_failed_get_many_error_never_contains_stdout() {
        let res = with_fake(&format!("bad:{}", entry_line("a", JSON)), |_| WinCredStore.get_many(&["a"]));
        let e = format!("{:#}", res[0].as_ref().unwrap_err());
        assert!(e.contains("oops") && !e.contains("sk-abc") && !e.contains(&BASE64.encode(JSON)), "{e}");
    }
}
