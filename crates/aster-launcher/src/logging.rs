// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Size above which a log file is rotated to `<name>.1` (see
/// [`rotate_if_larger`]). The launcher runs 24/7, so without a cap
/// `launcher.log` (never truncated) and a child log within one long session
/// would grow without bound.
pub(crate) const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Appends one timestamped line to `path`, creating the file and any
/// missing parent directories if needed. Never panics: a logging failure
/// must not take down the launcher.
pub fn append_line(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(
        file,
        "[{}] {}",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        message
    );
}

/// `<path>.1`, built by appending to the file name (not `with_extension`,
/// which would turn `launcher.log` into `launcher.1`).
pub(crate) fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

/// Rotates `path` to `<path>.1` (replacing an older `.1`) when it is larger
/// than `max_bytes`. Keeps exactly one previous generation, so a log never
/// takes more than ~2 × `max_bytes` on disk. Never panics and ignores every
/// error (missing file, rename refused because another process holds the
/// file open, ...): rotation is housekeeping and must never stop the
/// launcher or a child spawn. Returns `true` if the file was rotated.
pub(crate) fn rotate_if_larger(path: &Path, max_bytes: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() <= max_bytes {
        return false;
    }
    let rotated = rotated_path(path);
    // `rename` replaces an existing target on both Unix and Windows, but an
    // explicit remove keeps that independent of the platform's rename
    // flavor; a failure here simply surfaces as a failed rename below.
    let _ = std::fs::remove_file(&rotated);
    std::fs::rename(path, &rotated).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lines_with_timestamp_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("test.log");

        append_line(&path, "first");
        append_line(&path, "second");

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("first"));
        assert!(lines[1].ends_with("second"));
        assert!(lines[0].starts_with('['));
    }

    #[test]
    fn rotated_path_appends_suffix_to_full_file_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            rotated_path(&dir.path().join("launcher.log")),
            dir.path().join("launcher.log.1")
        );
    }

    #[test]
    fn rotation_leaves_small_logs_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.log");
        std::fs::write(&path, vec![b'x'; 100]).unwrap();

        assert!(!rotate_if_larger(&path, 100));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 100);
        assert!(!rotated_path(&path).exists());
    }

    #[test]
    fn rotation_moves_large_log_to_dot_one_replacing_old_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.log");
        let rotated = rotated_path(&path);
        std::fs::write(&rotated, b"older generation").unwrap();
        std::fs::write(&path, vec![b'x'; 101]).unwrap();

        assert!(rotate_if_larger(&path, 100));
        assert!(!path.exists());
        assert_eq!(std::fs::read(&rotated).unwrap(), vec![b'x'; 101]);

        // Logging continues into a fresh file.
        append_line(&path, "after rotation");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with("after rotation\n")
        );
    }

    #[test]
    fn rotation_of_missing_file_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!rotate_if_larger(&dir.path().join("nope.log"), 0));
    }
}
