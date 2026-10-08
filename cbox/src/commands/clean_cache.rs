//! `cbox clean-cache` — force a re-pull of a re-pushed tag and sweep orphaned
//! image blobs, across every per-name home.
//!
//! BoxLite caches tag→digest immutably: `ImageStore::pull` returns the cached
//! manifest for a reference whenever its `image_index` row is present and
//! complete, without asking the registry. So a rebuilt `cbox-custom:latest` is
//! ignored until that row is gone. The SDK has no way to drop it — at boxlite
//! 0.10.5 `runtime.images()` is `pull` and `list` only, and the index lives in
//! the crate-private `db` module — so this is direct surgery on BoxLite's own
//! sqlite schema, ported from the justfile recipe it replaces. It is a
//! workaround for BoxLite's missing tag invalidation and should be deleted
//! once BoxLite exposes one.
//!
//! Disk-images are left alone on purpose: BoxLite does not record which image
//! a disk-image belongs to, so an orphaned one cannot be told apart from a
//! live one without risking a costly, or outright breaking, re-pull. `cbox up
//! -f` sweeps those instead, from the qcow2 backing-file chain.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config;

/// The blob directories under `<home>/images` whose files are named by digest
/// (`sha256-<hex>`, plus `.json` or `.tar.gz`). `disk-images` is deliberately
/// absent — see the module docs.
const BLOB_DIRS: [&str; 4] = ["manifests", "configs", "layers", "extracted"];

/// What one home's sweep did, for the one-line summary.
#[derive(Debug, Default, PartialEq)]
struct Swept {
    dropped_tag: bool,
    removed: usize,
}

pub fn run(reference: &str) -> Result<()> {
    let root = config::boxes_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        eprintln!("clean-cache: no box homes under {}; skipping", root.display());
        return Ok(());
    };

    // One bad home must not stop the rest being cleaned, but it must not
    // read as success either: report each failure and exit non-zero at the
    // end.
    let mut failed = 0;
    for entry in entries.flatten() {
        let home = entry.path();
        if !home.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        match sweep_home(&home, reference) {
            Ok(None) => {}
            Ok(Some(s)) if s == Swept::default() => {}
            Ok(Some(s)) => eprintln!(
                "clean-cache: {name}: {}removed {} unreferenced blob(s)",
                if s.dropped_tag { format!("dropped {reference}, ") } else { String::new() },
                s.removed,
            ),
            Err(e) => {
                eprintln!("clean-cache: {name}: {e:#}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} box home(s) could not be cleaned");
    }
    Ok(())
}

/// Clean one home. `Ok(None)` means it has no BoxLite database yet, which is
/// a skip, not an error.
fn sweep_home(home: &Path, reference: &str) -> Result<Option<Swept>> {
    let db = home.join("db/boxlite.db");
    if !db.is_file() {
        return Ok(None);
    }
    // OPEN_READ_WRITE without CREATE, so a path that vanished between the
    // check and here is an error rather than a fresh empty database.
    let mut conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("cannot open {}", db.display()))?;
    // A running `cbox up` holds this database open; wait out its write
    // locks rather than failing on the first one.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    // Drop the tag so the next pull goes to the registry, and read what is
    // still referenced, in one transaction so a failure leaves the index
    // as it was.
    let tx = conn.transaction()?;
    let dropped_tag = tx
        .execute("DELETE FROM image_index WHERE reference = ?1", [reference])
        .context("cannot drop the cached tag (has BoxLite's image_index schema changed?)")?
        > 0;

    // Any error reading the keep-set has to abort the sweep: an empty set
    // would mean "nothing is referenced" and delete every blob.
    let keep = referenced_blobs(&tx)
        .context("cannot read referenced digests (has BoxLite's image_index schema changed?)")?;
    tx.commit()?;

    let mut removed = 0;
    let images = home.join("images");
    for dir in BLOB_DIRS {
        let Ok(files) = std::fs::read_dir(images.join(dir)) else { continue };
        for f in files.flatten() {
            let path = f.path();
            let file_name = f.file_name().to_string_lossy().into_owned();
            if keep.contains(blob_key(&file_name)) {
                continue;
            }
            let result = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            result.with_context(|| format!("cannot remove {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(Some(Swept { dropped_tag, removed }))
}

/// Every digest still referenced by a remaining image, in on-disk filename
/// form (`sha256:<hex>` → `sha256-<hex>`). `layers` is a JSON array of
/// digests, as BoxLite's own `ImageIndexStore` writes it.
fn referenced_blobs(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT manifest_digest, config_digest, layers FROM image_index")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut keep = HashSet::new();
    for row in rows {
        let (manifest, config, layers) = row?;
        let layers: Vec<String> = serde_json::from_str(&layers)
            .with_context(|| format!("malformed layers for manifest {manifest}"))?;
        keep.extend([manifest, config].into_iter().chain(layers).map(|d| d.replace(':', "-")));
    }
    Ok(keep)
}

/// A blob file's digest key: its name minus the `.json` or `.tar.gz` suffix
/// BoxLite's `ImageStorage` gives manifests/configs and layer tarballs.
/// Extracted layers are bare directories already.
fn blob_key(file_name: &str) -> &str {
    file_name
        .strip_suffix(".json")
        .or_else(|| file_name.strip_suffix(".tar.gz"))
        .unwrap_or(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const CUSTOM: &str = "localhost:5551/library/cbox-custom:latest";

    /// A home laid out the way BoxLite 0.10.5 lays one out, with two images
    /// indexed: the custom one (its own manifest/config, one layer of its own
    /// and one shared with debian) and debian.
    fn fixture(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("cbox-test-clean-cache-{name}"));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("db")).unwrap();
        let conn = Connection::open(home.join("db/boxlite.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE image_index (
                 reference TEXT PRIMARY KEY NOT NULL,
                 manifest_digest TEXT NOT NULL,
                 config_digest TEXT NOT NULL,
                 layers TEXT NOT NULL,
                 cached_at TEXT NOT NULL,
                 complete INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        let insert = "INSERT INTO image_index VALUES (?1, ?2, ?3, ?4, '2026-01-01', 1)";
        conn.execute(insert, [CUSTOM, "sha256:m1", "sha256:c1", r#"["sha256:l1","sha256:shared"]"#])
            .unwrap();
        conn.execute(insert, ["debian:bookworm", "sha256:m2", "sha256:c2", r#"["sha256:shared"]"#])
            .unwrap();

        let img = home.join("images");
        for (dir, file) in [
            ("manifests", "sha256-m1.json"),
            ("manifests", "sha256-m2.json"),
            ("configs", "sha256-c1.json"),
            ("configs", "sha256-c2.json"),
            ("layers", "sha256-l1.tar.gz"),
            ("layers", "sha256-shared.tar.gz"),
            ("disk-images", "sha256-c1.ext4"),
        ] {
            std::fs::create_dir_all(img.join(dir)).unwrap();
            std::fs::write(img.join(dir).join(file), "x").unwrap();
        }
        for dir in ["sha256-l1", "sha256-shared"] {
            std::fs::create_dir_all(img.join("extracted").join(dir).join("bin")).unwrap();
        }
        home
    }

    fn exists(home: &Path, rel: &str) -> bool {
        home.join("images").join(rel).exists()
    }

    #[test]
    fn drops_the_tag_and_sweeps_only_its_unshared_blobs() {
        let home = fixture("sweep");
        let swept = sweep_home(&home, CUSTOM).unwrap().unwrap();
        assert_eq!(swept, Swept { dropped_tag: true, removed: 4 });

        for gone in [
            "manifests/sha256-m1.json",
            "configs/sha256-c1.json",
            "layers/sha256-l1.tar.gz",
            "extracted/sha256-l1",
        ] {
            assert!(!exists(&home, gone), "{gone} should be swept");
        }
        for kept in [
            "manifests/sha256-m2.json",
            "configs/sha256-c2.json",
            "layers/sha256-shared.tar.gz",
            "extracted/sha256-shared",
        ] {
            assert!(exists(&home, kept), "{kept} is still referenced by debian");
        }

        let conn = Connection::open(home.join("db/boxlite.db")).unwrap();
        let refs: Vec<String> = conn
            .prepare("SELECT reference FROM image_index")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(refs, ["debian:bookworm"]);
    }

    #[test]
    fn disk_images_are_never_touched_even_when_orphaned() {
        let home = fixture("disk-images");
        sweep_home(&home, CUSTOM).unwrap();
        assert!(exists(&home, "disk-images/sha256-c1.ext4"));
    }

    #[test]
    fn a_second_run_is_a_no_op() {
        let home = fixture("idempotent");
        sweep_home(&home, CUSTOM).unwrap();
        assert_eq!(sweep_home(&home, CUSTOM).unwrap(), Some(Swept::default()));
    }

    #[test]
    fn a_home_without_a_database_is_skipped() {
        let home = std::env::temp_dir().join("cbox-test-clean-cache-no-db");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(sweep_home(&home, CUSTOM).unwrap(), None);
        assert!(!home.join("db/boxlite.db").exists(), "must not create a database");
    }

    #[test]
    fn an_unexpected_schema_fails_without_deleting_anything() {
        // The shell recipe piped its SELECTs into the keep-list, so a failed
        // query read as "nothing referenced" and swept every blob. Here a
        // schema the queries don't fit has to stop the sweep instead.
        let home = fixture("schema-drift");
        let conn = Connection::open(home.join("db/boxlite.db")).unwrap();
        conn.execute_batch("ALTER TABLE image_index RENAME COLUMN layers TO layer_digests;").unwrap();
        drop(conn);

        assert!(sweep_home(&home, CUSTOM).is_err());
        let conn = Connection::open(home.join("db/boxlite.db")).unwrap();
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM image_index", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "the tag drop must roll back with the failed sweep");
        assert!(exists(&home, "manifests/sha256-m2.json"));
        assert!(exists(&home, "extracted/sha256-shared"));
    }

    #[test]
    fn blob_keys_strip_only_the_known_suffixes() {
        assert_eq!(blob_key("sha256-abc.json"), "sha256-abc");
        assert_eq!(blob_key("sha256-abc.tar.gz"), "sha256-abc");
        assert_eq!(blob_key("sha256-abc"), "sha256-abc");
    }
}
