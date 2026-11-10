// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Size above which a log file is rotated to `<name>.1` (see
/// [`rotate_if_larger`]). The launcher runs 24/7, so without a cap
/// `launcher.log` (never truncated) and a child log within one long session
/// would grow without bound.
pub(crate) const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// How often an identical, consecutive error line is re-logged while it
/// keeps repeating (see [`LogDedup`]): often enough to show the problem is
/// still there, rare enough not to flood the log.
const DEDUP_RELOG_AFTER: Duration = Duration::from_secs(60 * 60);

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

/// What [`LogDedup::decide`] wants written for a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DedupDecision {
    /// Write the message; `suppressed` identical lines were swallowed since
    /// the previous message was last written (0 when nothing was dropped).
    Write { suppressed: u32 },
    /// Same as the previous message and too soon to repeat it: skip.
    Suppress,
}

/// Suppresses identical consecutive error lines from a periodic task (e.g.
/// a `display.state` write failing on every 2s heartbeat would otherwise
/// add ~43,000 lines a day). A repeated line is written again at most once
/// per [`DEDUP_RELOG_AFTER`] with the number of suppressed repeats, and a
/// [`LogDedup::reset`] (the operation succeeded again) lets the next
/// failure through immediately. Never panics: a poisoned lock is recovered.
#[derive(Default)]
pub(crate) struct LogDedup {
    state: Mutex<Option<DedupState>>,
}

struct DedupState {
    message: String,
    last_written: Instant,
    suppressed: u32,
}

impl LogDedup {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Decides (given `now`) whether `message` should be written, updating
    /// the dedup state. Kept separate from the I/O so it is unit-testable
    /// without sleeping.
    pub(crate) fn decide(&self, message: &str, now: Instant) -> DedupDecision {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = guard.as_mut()
            && state.message == message
        {
            if now.saturating_duration_since(state.last_written) < DEDUP_RELOG_AFTER {
                state.suppressed = state.suppressed.saturating_add(1);
                return DedupDecision::Suppress;
            }
            let suppressed = state.suppressed;
            state.last_written = now;
            state.suppressed = 0;
            return DedupDecision::Write { suppressed };
        }
        // A new message: report how many repeats of the previous one were
        // swallowed since it was last written.
        let suppressed = guard.as_ref().map_or(0, |s| s.suppressed);
        *guard = Some(DedupState {
            message: message.to_string(),
            last_written: now,
            suppressed: 0,
        });
        DedupDecision::Write { suppressed }
    }

    /// Appends `message` to `path` unless it repeats the previous message
    /// (see [`LogDedup`]).
    pub(crate) fn append_line(&self, path: &Path, message: &str) {
        match self.decide(message, Instant::now()) {
            DedupDecision::Suppress => {}
            DedupDecision::Write { suppressed: 0 } => append_line(path, message),
            DedupDecision::Write { suppressed } => append_line(
                path,
                &format!("{message} (previous error repeated {suppressed} more times)"),
            ),
        }
    }

    /// Forgets the last message (call after the operation succeeds), so
    /// the next failure is logged right away.
    pub(crate) fn reset(&self) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = None;
    }
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

    #[test]
    fn dedup_suppresses_identical_consecutive_lines() {
        let dedup = LogDedup::new();
        let t0 = Instant::now();
        assert_eq!(
            dedup.decide("write failed", t0),
            DedupDecision::Write { suppressed: 0 }
        );
        for i in 1..=5 {
            assert_eq!(
                dedup.decide("write failed", t0 + Duration::from_secs(2 * i)),
                DedupDecision::Suppress
            );
        }
        // A different message gets through and reports the swallowed repeats.
        assert_eq!(
            dedup.decide("other failure", t0 + Duration::from_secs(12)),
            DedupDecision::Write { suppressed: 5 }
        );
    }

    #[test]
    fn dedup_relogs_a_persistent_error_periodically() {
        let dedup = LogDedup::new();
        let t0 = Instant::now();
        dedup.decide("write failed", t0);
        assert_eq!(
            dedup.decide("write failed", t0 + Duration::from_secs(2)),
            DedupDecision::Suppress
        );
        assert_eq!(
            dedup.decide("write failed", t0 + DEDUP_RELOG_AFTER),
            DedupDecision::Write { suppressed: 1 }
        );
    }

    #[test]
    fn dedup_reset_lets_the_next_failure_through() {
        let dedup = LogDedup::new();
        let t0 = Instant::now();
        dedup.decide("write failed", t0);
        dedup.reset();
        assert_eq!(
            dedup.decide("write failed", t0 + Duration::from_secs(2)),
            DedupDecision::Write { suppressed: 0 }
        );
    }

    #[test]
    fn dedup_append_line_writes_once_for_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.log");
        let dedup = LogDedup::new();
        for _ in 0..10 {
            dedup.append_line(&path, "failed to write display.state");
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }
}
