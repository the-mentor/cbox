//! THROWAWAY SPIKE (branch spike/keyring-wsl): a custom `keyring-core` store that
//! keeps secrets in Windows Credential Manager by shelling out to powershell.exe
//! through WSL interop. Run `cargo run` inside WSL and paste the output back.
use keyring_core::api::{Credential, CredentialApi, CredentialStoreApi};
use keyring_core::{Entry, Error, Result, set_default_store};
use std::any::Any;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

const SCRIPT: &str = include_str!("wincred.ps1");

struct WinCredStore {
    script: PathBuf,
}

struct WinCred {
    script: PathBuf,
    target: String,
}

fn plat(msg: impl Into<String>) -> Error {
    Error::PlatformFailure(msg.into().into())
}

impl WinCred {
    fn run(&self, op: &str, stdin: Option<&[u8]>) -> Result<Vec<u8>> {
        let win = Command::new("wslpath").arg("-w").arg(&self.script).output().map_err(|e| plat(format!("wslpath: {e}")))?;
        let win = String::from_utf8_lossy(&win.stdout).trim().to_string();
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", &win, op, &self.target])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| plat(format!("powershell.exe: {e}")))?;
        // The secret travels over stdin only, never argv.
        let mut sin = child.stdin.take().unwrap();
        if let Some(b) = stdin {
            sin.write_all(b).map_err(|e| plat(e.to_string()))?;
        }
        drop(sin);
        let out = child.wait_with_output().map_err(|e| plat(e.to_string()))?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let err = String::from_utf8_lossy(&out.stderr).replace('\r', "");
        // Win32 ERROR_NOT_FOUND (1168) surfaces as "Element not found".
        if err.contains("Element not found") {
            return Err(Error::NoEntry);
        }
        Err(plat(format!("exit {:?}: {}", out.status.code(), err.trim())))
    }
}

impl CredentialApi for WinCred {
    fn set_secret(&self, secret: &[u8]) -> Result<()> {
        std::str::from_utf8(secret).map_err(|_| Error::Invalid("secret".into(), "must be UTF-8".into()))?;
        self.run("set", Some(secret)).map(|_| ())
    }
    fn get_secret(&self) -> Result<Vec<u8>> {
        self.run("get", None)
    }
    fn delete_credential(&self) -> Result<()> {
        self.run("del", None).map(|_| ())
    }
    fn get_credential(&self) -> Result<Option<Arc<Credential>>> {
        Ok(None)
    }
    fn get_specifiers(&self) -> Option<(String, String)> {
        None
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl CredentialStoreApi for WinCredStore {
    fn vendor(&self) -> String {
        "cbox spike: powershell.exe -> Windows Credential Manager".into()
    }
    fn id(&self) -> String {
        "wincred-spike-0".into()
    }
    fn build(&self, service: &str, user: &str, _: Option<&HashMap<&str, &str>>) -> Result<Entry> {
        Ok(Entry::new_with_credential(Arc::new(WinCred {
            script: self.script.clone(),
            target: format!("cbox/{service}/{user}"),
        })))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Windows processes started through interop show up in /proc with their argv;
/// scan for the secret there while a call is in flight.
fn secret_in_any_cmdline(secret: &str) -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else { return false };
    dir.flatten().any(|e| {
        std::fs::read(e.path().join("cmdline"))
            .map(|b| String::from_utf8_lossy(&b).contains(secret))
            .unwrap_or(false)
    })
}

fn main() {
    let wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.to_lowercase().contains("microsoft"))
        .unwrap_or(false);
    println!("wsl detected: {wsl}");

    let dir = std::env::temp_dir().join(format!("cbox-wincred-spike-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("wincred.ps1");
    std::fs::write(&script, SCRIPT).unwrap();
    set_default_store(Arc::new(WinCredStore { script }));

    let secret = "sp\u{e4}ss-w\u{f6}rd-\u{20ac}-\u{5e9}\u{5dc}\u{5d5}\u{5dd}-SPIKEMARK";
    let mut ok = true;
    let mut check = |name: &str, pass: bool, detail: String| {
        println!("{} {name}: {detail}", if pass { "PASS" } else { "FAIL" });
        ok &= pass;
    };

    let e = Entry::new("cbox-spike", "roundtrip").unwrap();
    let _ = e.delete_credential(); // clean slate; may be NoEntry

    let t = Instant::now();
    let watcher = std::thread::spawn(move || {
        let mut seen = false;
        for _ in 0..200 {
            seen |= secret_in_any_cmdline("SPIKEMARK");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        seen
    });
    let r = e.set_password(secret);
    check("set", r.is_ok(), format!("{r:?} in {:?}", t.elapsed()));

    let t = Instant::now();
    let got = e.get_password();
    check("get matches (unicode)", matches!(got.as_deref(), Ok(s) if s == secret), format!("{:?} in {:?}", got.as_ref().map(|_| "<value>"), t.elapsed()));

    let seen = watcher.join().unwrap();
    check("secret absent from every /proc/*/cmdline", !seen, format!("seen: {seen}"));

    let r = e.delete_credential();
    check("delete", r.is_ok(), format!("{r:?}"));
    let r = e.get_password();
    check("get after delete is NoEntry", matches!(r, Err(Error::NoEntry)), format!("{r:?}"));
    let r = e.delete_credential();
    check("delete missing is NoEntry", matches!(r, Err(Error::NoEntry)), format!("{r:?}"));

    let _ = std::fs::remove_dir_all(&dir);
    println!("{}", if ok { "ALL PASS" } else { "SOME FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
