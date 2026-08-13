// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Deserialize;
use std::path::Path;

/// Allowed sensor refresh intervals, in seconds. A single `refresh_time`
/// value is applied to both `aster-sysinfo` and `hwbridge`.
pub const REFRESH_OPTIONS: [u16; 4] = [2, 5, 10, 30];

/// Default shared refresh interval when no (valid) value is configured.
pub const DEFAULT_REFRESH_SECS: u16 = 5;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub monitor_config: String,
    /// Shared sensor refresh interval, in seconds, applied to BOTH
    /// aster-sysinfo and hwbridge. Must be one of [`REFRESH_OPTIONS`].
    /// Takes precedence over the legacy per-process keys below.
    pub refresh_time: Option<u16>,
    /// Legacy per-process override for aster-sysinfo, kept for backward
    /// compatibility. Only used when `refresh_time` is not configured.
    pub sysinfo_refresh: Option<u16>,
    /// Legacy per-process override for hwbridge, kept for backward
    /// compatibility. Only used when `refresh_time` is not configured.
    pub hwbridge_refresh: Option<u16>,
    /// On wake from sleep, disable + re-enable the AOOSTAR USB UART (force
    /// re-enumeration) before respawning children — the automated version of
    /// the manual Device Manager fix. Set false if Task 1's fresh-open is
    /// already sufficient on your hardware.
    pub restart_uart_on_resume: bool,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            monitor_config: "Monitor3.json".to_string(),
            // None so the effective fallback (DEFAULT_REFRESH_SECS) applies
            // and legacy per-process keys still win when present.
            refresh_time: None,
            sysinfo_refresh: None,
            hwbridge_refresh: None,
            restart_uart_on_resume: true,
        }
    }
}

impl LauncherConfig {
    /// Loads from `path`. Falls back to [`LauncherConfig::default`] (as a
    /// whole, or per-field for a partially-specified file) if the file is
    /// missing or cannot be parsed. Never panics, never returns `Err` —
    /// there is no valid state for the launcher to refuse to start in.
    pub fn load(path: &Path) -> Self {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                crate::logging::append_line(
                    &path.with_file_name("launcher.log"),
                    &format!("launcher.toml is invalid, using defaults: {err}"),
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        cfg.sanitize_refresh_values(path);
        cfg
    }

    /// Effective refresh interval for aster-sysinfo: the shared
    /// `refresh_time` wins, falling back to the legacy `sysinfo_refresh`,
    /// then to [`DEFAULT_REFRESH_SECS`].
    pub fn sysinfo_refresh_effective(&self) -> u16 {
        self.refresh_time
            .or(self.sysinfo_refresh)
            .unwrap_or(DEFAULT_REFRESH_SECS)
    }

    /// Effective refresh interval for hwbridge: the shared `refresh_time`
    /// wins, falling back to the legacy `hwbridge_refresh`, then to
    /// [`DEFAULT_REFRESH_SECS`].
    pub fn hwbridge_refresh_effective(&self) -> u16 {
        self.refresh_time
            .or(self.hwbridge_refresh)
            .unwrap_or(DEFAULT_REFRESH_SECS)
    }

    /// Drops any refresh value that is not one of [`REFRESH_OPTIONS`] so an
    /// out-of-range value can never reach a child process (e.g. the old
    /// `0`-seconds permanent respawn loop). Invalid values are noted in
    /// `launcher.log` and the field reverts to "not configured", so a bad
    /// `refresh_time` still falls back to the legacy keys / default.
    fn sanitize_refresh_values(&mut self, path: &Path) {
        let log = path.with_file_name("launcher.log");
        if let Some(v) = self.refresh_time
            && !REFRESH_OPTIONS.contains(&v)
        {
            crate::logging::append_line(
                &log,
                &format!(
                    "refresh_time={v} is not one of the allowed refresh intervals {REFRESH_OPTIONS:?}; ignoring it"
                ),
            );
            self.refresh_time = None;
        }
        if let Some(v) = self.sysinfo_refresh
            && !REFRESH_OPTIONS.contains(&v)
        {
            crate::logging::append_line(
                &log,
                &format!(
                    "sysinfo_refresh={v} is not one of the allowed refresh intervals {REFRESH_OPTIONS:?}; ignoring it"
                ),
            );
            self.sysinfo_refresh = None;
        }
        if let Some(v) = self.hwbridge_refresh
            && !REFRESH_OPTIONS.contains(&v)
        {
            crate::logging::append_line(
                &log,
                &format!(
                    "hwbridge_refresh={v} is not one of the allowed refresh intervals {REFRESH_OPTIONS:?}; ignoring it"
                ),
            );
            self.hwbridge_refresh = None;
        }
    }
}

/// Rewrites the `refresh_time = <N>` line in the launcher.toml at `path`,
/// preserving every other line and comment (used by the tray's refresh
/// menu). Creates the file with just that option if it does not exist yet.
/// Does not validate `secs` — callers pass one of [`REFRESH_OPTIONS`].
pub fn set_refresh_time(path: &Path, secs: u16) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };

    let line = format!("refresh_time = {secs}");
    let mut rewritten = false;
    let mut out = String::with_capacity(text.len() + line.len() + 2);
    for src in text.split_inclusive('\n') {
        let key = src.trim().split('=').next().unwrap_or("").trim();
        if key == "refresh_time" {
            // keep the original indentation, drop any trailing comment
            let indent: String = src.chars().take_while(|c| c.is_whitespace()).collect();
            out.push_str(&indent);
            out.push_str(&line);
            if !src.ends_with('\n') {
                out.push('\n');
            }
            rewritten = true;
        } else {
            out.push_str(src);
        }
    }
    if !rewritten {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
        out.push('\n');
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    #[test]
    fn missing_file_returns_defaults() {
        let path = Path::new("this-file-does-not-exist.toml");
        assert_eq!(LauncherConfig::load(path), LauncherConfig::default());
    }

    #[test]
    fn empty_file_returns_defaults() {
        let file = write_temp("");
        assert_eq!(LauncherConfig::load(file.path()), LauncherConfig::default());
    }

    #[test]
    fn partial_file_fills_missing_fields_with_defaults() {
        let file = write_temp("sysinfo_refresh = 10\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.sysinfo_refresh, Some(10));
        assert_eq!(cfg.monitor_config, LauncherConfig::default().monitor_config);
        // no refresh_time and no hwbridge_refresh in the file
        assert_eq!(cfg.refresh_time, None);
        assert_eq!(cfg.hwbridge_refresh, None);
        assert_eq!(cfg.sysinfo_refresh_effective(), 10);
        assert_eq!(cfg.hwbridge_refresh_effective(), DEFAULT_REFRESH_SECS);
    }

    #[test]
    fn malformed_file_returns_defaults() {
        let file = write_temp("sysinfo_refresh = \"not a number\"\n");
        assert_eq!(LauncherConfig::load(file.path()), LauncherConfig::default());
    }

    #[test]
    fn partial_file_defaults_restart_uart_on_resume() {
        let file = write_temp("sysinfo_refresh = 9\n");
        let cfg = LauncherConfig::load(file.path());
        assert!(cfg.restart_uart_on_resume);
    }

    #[test]
    fn restart_uart_can_be_disabled_in_config() {
        let file = write_temp("restart_uart_on_resume = false\n");
        let cfg = LauncherConfig::load(file.path());
        assert!(!cfg.restart_uart_on_resume);
    }

    #[test]
    fn shared_refresh_time_applies_to_both_processes() {
        let file = write_temp("refresh_time = 10\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.sysinfo_refresh_effective(), 10);
        assert_eq!(cfg.hwbridge_refresh_effective(), 10);
    }

    #[test]
    fn refresh_time_takes_precedence_over_legacy_keys() {
        let file = write_temp("refresh_time = 10\nsysinfo_refresh = 2\nhwbridge_refresh = 30\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.sysinfo_refresh_effective(), 10);
        assert_eq!(cfg.hwbridge_refresh_effective(), 10);
    }

    #[test]
    fn legacy_keys_used_when_no_shared_refresh_time() {
        let file = write_temp("sysinfo_refresh = 2\nhwbridge_refresh = 30\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.sysinfo_refresh_effective(), 2);
        assert_eq!(cfg.hwbridge_refresh_effective(), 30);
    }

    #[test]
    fn invalid_refresh_time_falls_back_to_legacy_keys() {
        let file = write_temp("refresh_time = 7\nsysinfo_refresh = 2\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.refresh_time, None);
        assert_eq!(cfg.sysinfo_refresh_effective(), 2);
        assert_eq!(cfg.hwbridge_refresh_effective(), DEFAULT_REFRESH_SECS);
    }

    #[test]
    fn invalid_legacy_refresh_values_fall_back_to_default() {
        let file = write_temp("sysinfo_refresh = 0\nhwbridge_refresh = 3\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.sysinfo_refresh, None);
        assert_eq!(cfg.hwbridge_refresh, None);
        assert_eq!(cfg.sysinfo_refresh_effective(), DEFAULT_REFRESH_SECS);
        assert_eq!(cfg.hwbridge_refresh_effective(), DEFAULT_REFRESH_SECS);
    }

    #[test]
    fn set_refresh_time_updates_value_and_keeps_comments_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.toml");
        std::fs::write(
            &path,
            "# my config\nrefresh_time = 5\n\n# legacy\nsysinfo_refresh = 2\n",
        )
        .unwrap();

        set_refresh_time(&path, 30).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("refresh_time = 30"));
        assert!(!text.contains("refresh_time = 5"));
        assert!(text.contains("# legacy"));
        assert!(text.contains("sysinfo_refresh = 2"));

        let cfg = LauncherConfig::load(&path);
        assert_eq!(cfg.refresh_time, Some(30));
        assert_eq!(cfg.sysinfo_refresh_effective(), 30);
        assert_eq!(cfg.hwbridge_refresh_effective(), 30);
    }

    #[test]
    fn set_refresh_time_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.toml");

        set_refresh_time(&path, 2).unwrap();

        let cfg = LauncherConfig::load(&path);
        assert_eq!(cfg.refresh_time, Some(2));
        assert_eq!(cfg.sysinfo_refresh_effective(), 2);
        assert_eq!(cfg.hwbridge_refresh_effective(), 2);
    }

    #[test]
    fn set_refresh_time_appends_when_key_absent_but_other_content_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.toml");
        std::fs::write(&path, "monitor_config = \"Monitor3.json\"\n").unwrap();

        set_refresh_time(&path, 10).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("monitor_config = \"Monitor3.json\""));
        assert!(text.contains("refresh_time = 10"));
        let cfg = LauncherConfig::load(&path);
        assert_eq!(cfg.sysinfo_refresh_effective(), 10);
    }
}
