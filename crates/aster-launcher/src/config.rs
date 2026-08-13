// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub monitor_config: String,
    pub sysinfo_refresh: u16,
    pub hwbridge_refresh: u16,
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
            sysinfo_refresh: 2,
            hwbridge_refresh: 5,
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
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                crate::logging::append_line(
                    &path.with_file_name("launcher.log"),
                    &format!("launcher.toml is invalid, using defaults: {err}"),
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }
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
        let file = write_temp("sysinfo_refresh = 9\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.sysinfo_refresh, 9);
        assert_eq!(cfg.monitor_config, LauncherConfig::default().monitor_config);
        assert_eq!(
            cfg.hwbridge_refresh,
            LauncherConfig::default().hwbridge_refresh
        );
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
}
