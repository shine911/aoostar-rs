// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

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
}
