//! Listing and path-checking for the backup artifact tree.
//!
//! `/mnt/backup/backups` is flat-ish by design: root files plus one level of
//! per-app directories (`immich/`, `nextcloud/`, `stalwart/`, `logs/`). Two
//! machine-managed trees are never walked — `home-storage` is the rsync mirror
//! of `$STORAGE_HOME` and walking it produced a ~360 MB HTML page, `preserves`
//! has its own UI.

use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::config::BACKUP_SKIP_DIRS;

/// Files are collected from the root and its direct subdirectories only.
pub const MAX_DEPTH: usize = 2;
/// Safety net if a subdirectory still grows unexpectedly.
pub const MAX_FILES: usize = 500;

#[derive(Debug, Clone)]
pub struct BackupFile {
    pub rel: String,
    pub name: String,
    pub size: u64,
    pub mtime_ms: i64,
}

pub fn list(root: &Path) -> Vec<BackupFile> {
    let mut out = Vec::new();
    if root.is_dir() {
        walk(root, root, 0, &mut out);
    }
    out.sort_by_key(|file| std::cmp::Reverse(file.mtime_ms));
    out.truncate(MAX_FILES);
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<BackupFile>) {
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        if meta.is_dir() {
            if BACKUP_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(root, &path, depth + 1, out);
        } else if meta.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|delta| delta.as_millis() as i64)
                .unwrap_or(0);
            out.push(BackupFile {
                rel: rel.to_string_lossy().to_string(),
                name,
                size: meta.len(),
                mtime_ms,
            });
        }
    }
}

/// Resolve a user-supplied relative path inside `root`, or `None` if it tries
/// to escape (absolute path, `..`, symlink pointing outside).
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    let relative = Path::new(rel);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let candidate = root.join(relative).canonicalize().ok()?;
    if candidate != root && candidate.starts_with(&root) {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, size: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; size]).unwrap();
    }

    #[test]
    fn walks_root_and_one_level_of_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("pkg_list_1.txt"), 10);
        touch(&root.join("immich/immich_db_1.dump"), 100);
        touch(&root.join("logs/pg_1.err"), 5);
        // Depth 2: not collected, not descended into.
        touch(&root.join("immich/deeper/file.dump"), 200);

        let files = list(root);
        let names: Vec<&str> = files.iter().map(|file| file.rel.as_str()).collect();
        assert!(names.contains(&"pkg_list_1.txt"));
        assert!(names.contains(&"immich/immich_db_1.dump"));
        assert!(names.contains(&"logs/pg_1.err"));
        assert!(!names.iter().any(|name| name.contains("deeper")));
    }

    #[test]
    fn machine_managed_trees_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("home-storage/current/some/file.txt"), 10);
        touch(&root.join("preserves/immich/1/db.dump"), 10);
        touch(&root.join("immich/keep.dump"), 10);

        let files = list(root);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel, "immich/keep.dump");
    }

    #[test]
    fn newest_first_and_capped() {
        use std::fs::FileTimes;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("old.dump"), 1);
        touch(&root.join("new.dump"), 1);

        let now = std::time::SystemTime::now();
        let old = now - std::time::Duration::from_secs(3600);
        let file = std::fs::OpenOptions::new().write(true).open(root.join("old.dump")).unwrap();
        file.set_times(FileTimes::new().set_modified(old)).unwrap();

        let files = list(root);
        assert_eq!(files[0].rel, "new.dump");
        assert_eq!(files[1].rel, "old.dump");
        assert!(files[0].mtime_ms > files[1].mtime_ms);
    }

    #[test]
    fn listing_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for index in 0..(MAX_FILES + 20) {
            touch(&root.join(format!("file_{index}.txt")), 1);
        }
        assert_eq!(list(root).len(), MAX_FILES);
    }

    #[test]
    fn traversal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("immich/keep.dump"), 10);
        assert!(safe_join(root, "immich/keep.dump").is_some());
        assert!(safe_join(root, "../etc/passwd").is_none());
        assert!(safe_join(root, "/etc/passwd").is_none());
        assert!(safe_join(root, "immich/../../etc/passwd").is_none());
        assert!(safe_join(root, "").is_none());
        // A path that does not exist cannot be resolved.
        assert!(safe_join(root, "missing.dump").is_none());
    }
}
