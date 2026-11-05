//! `cbox list` — every box across every per-name home.
//!
//! Homes are per box name, so listing means walking them all. Columns follow
//! `boxlite`'s own CLI (ID, IMAGE, STATUS, CREATED) plus one this repo needs
//! that the SDK cannot supply: ORIGIN, the directory a box was created from.
//! Names are derived from the git root, so without that column there is
//! nothing on screen tying a box back to the checkout it belongs to. The box
//! matching the current directory is additionally marked, since the derived
//! default name is otherwise invisible. SIZE is the whole per-name home (box
//! disk plus its image cache), since that is what `cbox down` deletes.

use anyhow::{Context, Result};
use boxlite::{BoxStatus, BoxliteOptions, BoxliteRuntime};

use crate::{config, naming, sidecar};

const NAME_W: usize = 20;
const ID_W: usize = 9;
const STATUS_W: usize = 10;
const IMAGE_W: usize = 24;
const CREATED_W: usize = 16;
const SIZE_W: usize = 8;
const ORIGIN_W: usize = 30;

pub async fn run(all: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let here = naming::resolve(None, &cwd).name;

    let root = config::box_home("_").parent().unwrap().to_path_buf();
    let Ok(entries) = std::fs::read_dir(&root) else {
        println!("cbox: no boxes yet");
        return Ok(());
    };

    let mut found = false;
    let mut header_printed = false;

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let home = entry.path();
        // Plain filesystem walk, so it works even when the home is locked.
        let size = human_size(disk_usage(&home));

        let runtime = match BoxliteRuntime::new(BoxliteOptions {
            home_dir: home.clone(),
            image_registries: vec![],
            image_registry_proxy: None,
        }) {
            Ok(r) => r,
            // A home locked by a running `cbox up` cannot be opened here —
            // the common case being a second terminal listing while the
            // first is still attached. Report it and move on rather than
            // aborting the whole listing.
            Err(_) => {
                if !header_printed {
                    print_header();
                    header_printed = true;
                }
                // A locked home can't be opened to read its status/sidecar,
                // so there's nothing but the name to show -- but it still
                // has to line up under the seven-column header, not just the
                // NAME cell, or a listing that mixes this with full rows
                // reads as broken. Reuse `format_row` so it's the same
                // layout, not a second hand-rolled one.
                let row = Row {
                    name: name.clone(),
                    id: String::new(),
                    status: "(in use)".into(),
                    image: String::new(),
                    created: String::new(),
                    size: size.clone(),
                    origin: String::new(),
                    marker: String::new(),
                };
                println!("{}", format_row(&row));
                found = true;
                continue;
            }
        };

        let origin = sidecar::read(&home).map(|s| s.origin).unwrap_or_default();

        for info in runtime.list_info().await.unwrap_or_default() {
            if !should_show(info.status, all) {
                continue;
            }
            if !header_printed {
                print_header();
                header_printed = true;
            }
            let row = Row {
                name: info.name.clone().unwrap_or_else(|| name.clone()),
                id: info.id.short().to_string(),
                status: info.status.to_string(),
                image: info.image.clone(),
                created: info.created_at.format("%Y-%m-%d %H:%M").to_string(),
                size: size.clone(),
                origin: origin.clone(),
                marker: marker_for(&name, &here).to_string(),
            };
            println!("{}", format_row(&row));
            found = true;
        }
    }

    if !found {
        println!("cbox: no boxes yet");
    }
    Ok(())
}

/// One rendered row's worth of already-stringified fields. Kept as owned
/// `String`s rather than borrowing from `BoxInfo` so `format_row` — the part
/// worth unit-testing — doesn't need a live runtime to call.
struct Row {
    name: String,
    id: String,
    status: String,
    image: String,
    created: String,
    size: String,
    origin: String,
    marker: String,
}

fn print_header() {
    let header = Row {
        name: "NAME".into(),
        id: "ID".into(),
        status: "STATUS".into(),
        image: "IMAGE".into(),
        created: "CREATED".into(),
        size: "SIZE".into(),
        origin: "ORIGIN".into(),
        marker: String::new(),
    };
    println!("{}", format_row(&header));
}

/// Whether a box's status earns it a row. `all` overrides the default of
/// hiding stopped boxes, which would otherwise accumulate forever since
/// `cbox down` — not stopping — is what removes them.
fn should_show(status: BoxStatus, all: bool) -> bool {
    all || status != BoxStatus::Stopped
}

/// The current-directory marker: visible only on the one box whose derived
/// name matches where `cbox list` was run from.
fn marker_for(box_name: &str, here: &str) -> &'static str {
    if box_name == here { " <- here" } else { "" }
}

/// Bytes actually allocated under `path` (blocks, not apparent length, since
/// box disks are sparse files). Symlinks are not followed; unreadable entries
/// count as zero rather than failing the listing.
fn disk_usage(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let own = meta.blocks() * 512;
    if !meta.is_dir() {
        return own;
    }
    let children = std::fs::read_dir(path)
        .map(|rd| rd.flatten().map(|e| disk_usage(&e.path())).sum())
        .unwrap_or(0);
    own + children
}

/// `du -h`-style: one decimal below 10 of a unit, whole numbers above.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 || v >= 10.0 {
        format!("{:.0}{}", v, UNITS[i])
    } else {
        format!("{:.1}{}", v, UNITS[i])
    }
}

/// Fit a value into a fixed-width column: truncate (with a trailing `…` so
/// truncation is visible) if it's too long, pad if it's short. This is what
/// keeps a long image reference or origin path from wrapping the row and
/// destroying the alignment of every column after it.
fn cell(s: &str, width: usize) -> String {
    format!("{:<width$}", truncate(s, width), width = width)
}

fn truncate(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Name and origin are the two things a human scans for, so they bookend the
/// row: name leftmost (first thing read), origin last (right before the
/// marker a reader is also hunting for).
fn format_row(row: &Row) -> String {
    format!(
        "{} {} {} {} {} {} {}{}",
        cell(&row.name, NAME_W),
        cell(&row.id, ID_W),
        cell(&row.status, STATUS_W),
        cell(&row.image, IMAGE_W),
        cell(&row.created, CREATED_W),
        cell(&row.size, SIZE_W),
        cell(&row.origin, ORIGIN_W),
        row.marker,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("hi", 5), "hi");
        assert_eq!(truncate("exact", 5), "exact");
    }

    #[test]
    fn truncate_marks_long_strings_with_a_trailing_ellipsis_at_the_fixed_width() {
        let out = truncate("hello world", 5);
        assert_eq!(out, "hell…");
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn truncate_handles_pathological_widths_without_panicking() {
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("hello", 1), "…");
    }

    #[test]
    fn truncate_is_safe_on_multi_byte_characters() {
        // `.chars()` makes this immune to the UTF-8 boundary panic that byte
        // slicing (`&s[..n]`) would risk here -- "café-résumé" has
        // multi-byte characters that don't fall on convenient byte
        // boundaries, and ORIGIN carries arbitrary filesystem paths.
        let out = truncate("café-résumé", 5);
        assert_eq!(out, "café…");
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn cell_pads_short_values_to_the_column_width() {
        assert_eq!(cell("hi", 5), "hi   ");
    }

    #[test]
    fn cell_truncates_long_values_rather_than_widening_the_column() {
        let out = cell("a-very-long-image-reference:latest", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'), "should be visibly truncated: {out:?}");
    }

    #[test]
    fn should_show_hides_stopped_boxes_unless_all_is_set() {
        assert!(!should_show(BoxStatus::Stopped, false));
        assert!(should_show(BoxStatus::Stopped, true));
        assert!(should_show(BoxStatus::Running, false));
        assert!(should_show(BoxStatus::Running, true));
    }

    #[test]
    fn human_size_uses_du_style_units() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(20 * 1024 * 1024), "20M");
        assert_eq!(
            human_size(3 * 1024 * 1024 * 1024 + 300 * 1024 * 1024),
            "3.3G"
        );
    }

    #[test]
    fn disk_usage_counts_nested_files_and_tolerates_missing_paths() {
        let dir = std::env::temp_dir().join(format!("cbox-du-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/f"), vec![1u8; 64 * 1024]).unwrap();
        let used = disk_usage(&dir);
        let missing = disk_usage(&dir.join("missing"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(used >= 64 * 1024, "got {used}");
        assert_eq!(missing, 0);
    }

    #[test]
    fn marker_for_flags_only_the_box_matching_the_current_directory() {
        assert_eq!(marker_for("cbox", "cbox"), " <- here");
        assert_eq!(marker_for("other-box", "cbox"), "");
    }

    #[test]
    fn format_row_renders_a_missing_origin_as_an_empty_cell_not_a_placeholder() {
        let row = Row {
            name: "my-box".into(),
            id: "abc12345".into(),
            status: "running".into(),
            image: "cbox-custom".into(),
            created: "2026-08-18 09:00".into(),
            size: "1.2G".into(),
            origin: String::new(),
            marker: " <- here".into(),
        };
        let rendered = format_row(&row);
        assert!(rendered.contains("my-box"));
        assert!(rendered.ends_with(" <- here"));
        // The empty origin cell is still full-width padding, not "N/A" or
        // similar — nothing but spaces where the path would be.
        assert!(rendered.contains(&" ".repeat(ORIGIN_W)));
    }

    #[test]
    fn in_use_row_still_lines_up_under_the_header() {
        // A locked home can only show a name and "(in use)" -- but it must
        // occupy the same seven cells as every other row, not a short
        // two-column line, or a listing that mixes both looks broken.
        let row = Row {
            name: "locked-box".into(),
            id: String::new(),
            status: "(in use)".into(),
            image: String::new(),
            created: String::new(),
            size: String::new(),
            origin: String::new(),
            marker: String::new(),
        };
        let rendered = format_row(&row);
        let header = format_row(&Row {
            name: "NAME".into(),
            id: "ID".into(),
            status: "STATUS".into(),
            image: "IMAGE".into(),
            created: "CREATED".into(),
            size: "SIZE".into(),
            origin: "ORIGIN".into(),
            marker: String::new(),
        });
        assert_eq!(
            rendered.len(),
            header.len(),
            "in-use row must be the same width as the header"
        );
        assert!(rendered.contains("locked-box"));
        assert!(rendered.contains("(in use)"));
    }
}
