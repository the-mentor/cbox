//! Renewal of BoxLite's per-box MITM CA.
//!
//! A box with secrets gets an HTTPS proxy on the host that re-signs traffic
//! with a CA BoxLite generates once and saves under
//! `<box home>/boxes/<box_id>/ca/{cert,key}.pem`. That CA is valid for only
//! 24 hours, and BoxLite reloads the saved one on every later start without
//! checking its expiry -- so a box older than a day fails every HTTPS request
//! that goes through the proxy. Deleting both files makes BoxLite generate a
//! fresh CA on the next start, and a stopped box re-trusts its CA on every
//! boot (an expired copy left in the guest's trust store is harmless). A
//! running box can't pick up a new CA at all: only a stop and start does.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

// ponytail: BoxLite 0.10.4 hard-codes a 24h lifetime (`net/ca.rs`,
// `not_after = now + 24h`) and writes `cert.pem` once, at generation, so the
// file's mtime + 24h is its expiry. Revisit if BoxLite changes either.
const CA_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// Renew a CA with less than this left: half its lifetime, so a box started
/// now keeps a working CA for at least half a day of use.
const RENEW_BELOW: Duration = Duration::from_secs(12 * 60 * 60);

/// Seconds left before each box's CA in `home` expires (negative once it
/// has), keyed by its `ca/` directory. Boxes without secrets have no `ca/`
/// and are skipped; so is a home with no boxes yet.
fn remaining(home: &Path, now: SystemTime) -> Result<Vec<(PathBuf, i64)>> {
    let boxes = home.join("boxes");
    let Ok(entries) = std::fs::read_dir(&boxes) else { return Ok(vec![]) };
    let mut out = vec![];
    for e in entries {
        let ca = e.with_context(|| format!("reading {}", boxes.display()))?.path().join("ca");
        let cert = ca.join("cert.pem");
        let modified = match std::fs::metadata(&cert) {
            Ok(m) => m.modified().with_context(|| format!("reading {}", cert.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {}", cert.display())),
        };
        let expires = modified + CA_LIFETIME;
        let left = match expires.duration_since(now) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        };
        out.push((ca, left));
    }
    Ok(out)
}

/// Delete every CA in `home` with less than 12 hours left, so BoxLite
/// generates a fresh one when the box next starts. Call only on a box that
/// is not running. Returns the `ca/` directories renewed.
pub fn renew_if_expiring(home: &Path) -> Result<Vec<PathBuf>> {
    renew_if_expiring_at(home, SystemTime::now())
}

fn renew_if_expiring_at(home: &Path, now: SystemTime) -> Result<Vec<PathBuf>> {
    let mut renewed = vec![];
    for (ca, left) in remaining(home, now)? {
        if left >= RENEW_BELOW.as_secs() as i64 {
            continue;
        }
        for f in ["cert.pem", "key.pem"] {
            let path = ca.join(f);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("deleting {}", path.display())),
            }
        }
        renewed.push(ca);
    }
    Ok(renewed)
}

/// Best-effort renewal for a box about to start: prints one line when it
/// renews, and only warns on failure.
pub fn renew_before_start(home: &Path, name: &str) {
    match renew_if_expiring(home) {
        Ok(renewed) if !renewed.is_empty() => {
            println!("cbox: renewed {name}'s MITM CA (was expiring)")
        }
        Ok(_) => {}
        Err(e) => eprintln!("cbox: warning: could not check {name}'s MITM CA for expiry: {e:#}"),
    }
}

/// Best-effort warning for a box that is already running, whose CA can't be
/// renewed without a restart that would kill the attached session.
pub fn warn_if_expiring(home: &Path, name: &str) {
    match remaining(home, SystemTime::now()) {
        Ok(cas) => {
            if let Some(msg) = cas.iter().map(|(_, left)| *left).min().and_then(|l| warning(name, l)) {
                eprintln!("{msg}");
            }
        }
        Err(e) => eprintln!("cbox: warning: could not check {name}'s MITM CA for expiry: {e:#}"),
    }
}

fn warning(name: &str, left: i64) -> Option<String> {
    if left >= RENEW_BELOW.as_secs() as i64 {
        return None;
    }
    let when = if left <= 0 {
        "has expired".to_string()
    } else {
        format!("expires in {}h", (left + 3599) / 3600)
    };
    Some(format!(
        "cbox: warning: {name}'s MITM CA {when} (HTTPS via secrets will fail after that); \
         stop the box and run cbox up again to renew it"
    ))
}

#[cfg(test)]
mod tests {
    use super::{renew_if_expiring_at, warning, CA_LIFETIME};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    const HOUR: Duration = Duration::from_secs(3600);

    fn make_ca(home: &Path, id: &str, written: SystemTime) -> std::path::PathBuf {
        let ca = home.join("boxes").join(id).join("ca");
        std::fs::create_dir_all(&ca).unwrap();
        for f in ["cert.pem", "key.pem"] {
            let path = ca.join(f);
            std::fs::write(&path, b"pem").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(written)
                .unwrap();
        }
        ca
    }

    #[test]
    fn renews_a_ca_past_half_its_life_and_keeps_a_fresh_one() {
        let home = std::env::temp_dir().join(format!("cbox-ca-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let now = SystemTime::now();
        let old = make_ca(&home, "old", now - 13 * HOUR);
        let expired = make_ca(&home, "expired", now - 2 * CA_LIFETIME);
        let fresh = make_ca(&home, "fresh", now - HOUR);
        // A box without secrets has no ca/ folder.
        std::fs::create_dir_all(home.join("boxes/nosecrets")).unwrap();

        let mut renewed = renew_if_expiring_at(&home, now).unwrap();
        renewed.sort();
        assert_eq!(renewed, vec![expired.clone(), old.clone()]);
        for ca in [&old, &expired] {
            assert!(!ca.join("cert.pem").exists());
            assert!(!ca.join("key.pem").exists());
        }
        assert!(fresh.join("cert.pem").exists());
        assert!(fresh.join("key.pem").exists());

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_home_with_no_boxes_is_fine() {
        let home = std::env::temp_dir().join(format!("cbox-ca-test-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(renew_if_expiring_at(&home, SystemTime::now()).unwrap().is_empty());
        std::fs::create_dir_all(&home).unwrap();
        assert!(renew_if_expiring_at(&home, SystemTime::now()).unwrap().is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn warns_only_when_expiring_and_says_when_expired() {
        assert_eq!(warning("demo", 20 * 3600), None);
        let soon = warning("demo", 5 * 3600 - 10).unwrap();
        assert!(soon.contains("demo's MITM CA expires in 5h"), "{soon}");
        let gone = warning("demo", -60).unwrap();
        assert!(gone.contains("has expired"), "{gone}");
    }
}
