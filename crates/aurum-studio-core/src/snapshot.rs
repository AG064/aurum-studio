//! Bounded source snapshots shared by preview exports and rendered playtests.
use crate::hash::Sha256;
use std::io::Read;
use std::path::Path;

fn ignored(name: &str) -> bool {
    (name.starts_with('.') && name != ".gdignore" && name != ".cargo")
        || name == "credentials.toml"
        || matches!(
            name,
            "dist" | "target" | "node_modules" | "logs" | "test-results" | "playwright-report"
        )
}

/// Whether the path participates in the public source snapshot, not private state.
pub fn includes(relative: &str) -> bool {
    relative
        .replace('\\', "/")
        .split('/')
        .all(|part| !ignored(part))
}

/// Bounded source inventory without reading file content.
pub fn inventory(source: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut entries = Vec::new();
    visit(
        source,
        source,
        0,
        false,
        &mut 0,
        &mut 0,
        &mut |path, relative| {
            entries.push((
                relative.to_string(),
                std::fs::metadata(path)
                    .map_err(|error| error.to_string())?
                    .len(),
            ));
            Ok(())
        },
    )?;
    Ok(entries)
}

fn visit(
    root: &Path,
    directory: &Path,
    depth: usize,
    ignore_live_data: bool,
    count: &mut usize,
    bytes: &mut u64,
    callback: &mut impl FnMut(&Path, &str) -> Result<(), String>,
) -> Result<(), String> {
    if depth > 24 {
        return Err("Project nesting exceeds the snapshot limit".into());
    }
    let mut entries = std::fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_str().ok_or("Snapshot filenames must be UTF-8")?;
        if ignored(name) {
            continue;
        }
        // Live data does not affect code freshness. Avoid opening even a
        // metadata handle: it can contend with Windows atomic replacement.
        // DirEntry's type also keeps a directory or symlink with this name
        // under the ordinary traversal and confinement rules.
        if ignore_live_data
            && name == "tuning.json"
            && entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("Snapshots do not follow symlinks: {name}"));
        }
        if metadata.is_dir() {
            visit(
                root,
                &entry.path(),
                depth + 1,
                ignore_live_data,
                count,
                bytes,
                callback,
            )?;
        } else if metadata.is_file() {
            *count += 1;
            *bytes = bytes.saturating_add(metadata.len());
            if *count > 20_000 || *bytes > 4 * 1024 * 1024 * 1024 {
                return Err("Project exceeds the bounded snapshot limit".into());
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Snapshot filenames must be UTF-8")?
                .replace('\\', "/");
            callback(&path, &relative)?;
        } else {
            return Err(format!("Unsupported snapshot entry: {name}"));
        }
    }
    Ok(())
}

pub fn copy(source: &Path, destination: &Path) -> Result<(), String> {
    copy_excluding(source, destination, &[])
}

/// Omit tooling from newly created snapshots. Never delete or alter source entries.
pub fn copy_excluding(source: &Path, destination: &Path, excluded: &[&str]) -> Result<(), String> {
    std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    visit(
        source,
        source,
        0,
        false,
        &mut 0,
        &mut 0,
        &mut |path, relative| {
            if excluded
                .iter()
                .any(|prefix| relative == *prefix || relative.starts_with(&format!("{prefix}/")))
            {
                return Ok(());
            }
            let target = destination.join(relative);
            std::fs::create_dir_all(target.parent().ok_or("Missing snapshot parent")?)
                .map_err(|e| e.to_string())?;
            std::fs::copy(path, target).map_err(|e| e.to_string())?;
            Ok(())
        },
    )
}

/// Hash paths and bytes, not timestamps: same-size edits and deletes invalidate a build.
/// The root tuning file is live data and intentionally does not invalidate code exports.
pub fn fingerprint(source: &Path) -> Result<String, String> {
    fingerprint_with_live_data(source, false)
}

/// Inspection caches include live files because script initializers can read
/// them. Preview code freshness intentionally uses the lighter contract above.
pub fn fingerprint_all(source: &Path) -> Result<String, String> {
    fingerprint_with_live_data(source, true)
}

fn fingerprint_with_live_data(source: &Path, include_live: bool) -> Result<String, String> {
    let mut hash = Sha256::new();
    visit(
        source,
        source,
        0,
        !include_live,
        &mut 0,
        &mut 0,
        &mut |path, relative| {
            if !include_live && (relative == "tuning.json" || relative.ends_with("/tuning.json")) {
                return Ok(());
            }
            hash.update(&(relative.len() as u64).to_le_bytes());
            hash.update(relative.as_bytes());
            let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
            hash.update(
                &file
                    .metadata()
                    .map_err(|e| e.to_string())?
                    .len()
                    .to_le_bytes(),
            );
            let mut buffer = [0; 64 * 1024];
            loop {
                let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if size == 0 {
                    break;
                }
                hash.update(&buffer[..size]);
            }
            Ok(())
        },
    )?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_write_staging_does_not_enter_fingerprints_or_copies() {
        let root = std::env::temp_dir().join(format!(
            "aurum-snapshot-staging-{}",
            crate::random::session_id()
        ));
        let source = root.join("source");
        let destination = root.join("copy");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("main.gd"), "extends Node").unwrap();
        std::fs::write(source.join("tuning.json"), "{}").unwrap();
        let revision = fingerprint(&source).unwrap();
        let staging = crate::files::sibling_temp(&source.join("tuning.json"));
        std::fs::write(&staging, "partially written live data").unwrap();
        assert_eq!(fingerprint(&source).unwrap(), revision);
        copy(&source, &destination).unwrap();
        assert!(!destination.join(staging.file_name().unwrap()).exists());
        assert_eq!(
            std::fs::read(destination.join("tuning.json")).unwrap(),
            b"{}"
        );
        let directory = source.join("assets/tuning.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("visible.gd"), "extends Node").unwrap();
        assert_ne!(fingerprint(&source).unwrap(), revision);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_atomic_writes_do_not_break_concurrent_source_scans() {
        let root = std::env::temp_dir().join(format!(
            "aurum-snapshot-live-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.gd"), "extends Node").unwrap();
        crate::files::write_atomic(&root.join("tuning.json"), b"{}").unwrap();
        let revision = fingerprint(&root).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writer_root = root.clone();
        let writer_barrier = barrier.clone();
        let writer = std::thread::spawn(move || {
            writer_barrier.wait();
            for value in 0..100 {
                crate::files::write_atomic(
                    &writer_root.join("tuning.json"),
                    format!("{{\"player_speed\":{value}}}").as_bytes(),
                )
                .unwrap();
            }
        });
        barrier.wait();
        for _ in 0..100 {
            assert_eq!(fingerprint(&root).unwrap(), revision);
        }
        writer.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revisions_detect_same_size_edits_deletes_and_ignore_live_or_generated_data() {
        let root =
            std::env::temp_dir().join(format!("aurum-snapshot-{}", crate::random::session_id()));
        std::fs::create_dir_all(root.join(".godot")).unwrap();
        std::fs::write(root.join("main.gd"), "one").unwrap();
        let first = fingerprint(&root).unwrap();
        std::fs::write(root.join("tuning.json"), "{}").unwrap();
        let complete = fingerprint_all(&root).unwrap();
        std::fs::write(root.join("tuning.json"), "{\"speed\":9}").unwrap();
        assert_ne!(fingerprint_all(&root).unwrap(), complete);
        std::fs::write(root.join(".godot/cache"), "cached").unwrap();
        assert_eq!(fingerprint(&root).unwrap(), first);
        std::fs::write(root.join("main.gd"), "two").unwrap();
        assert_ne!(fingerprint(&root).unwrap(), first);
        std::fs::remove_file(root.join("main.gd")).unwrap();
        assert_ne!(fingerprint(&root).unwrap(), first);
        std::fs::remove_dir_all(root).unwrap();
    }
}
