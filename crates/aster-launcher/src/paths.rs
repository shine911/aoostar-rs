// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::PathBuf;

/// All filesystem locations used by the launcher.  Linux follows the XDG
/// directory specification; environment overrides make the resolver useful
/// for tests and portable development builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherPaths {
    pub assets: PathBuf,
    pub binaries: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub runtime: PathBuf,
    pub logs: PathBuf,
    pub lock: PathBuf,
    pub display_state: PathBuf,
    pub stuck_file: PathBuf,
    pub sensors: PathBuf,
}

impl LauncherPaths {
    pub fn linux() -> Self {
        Self::from_lookup(|name| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
    }

    /// Resolve paths from a supplied environment lookup. Keeping lookup
    /// injectable makes fallback and XDG precedence tests deterministic
    /// without mutating the process environment (which is unsafe in 2024).
    pub fn from_lookup<F>(lookup: F) -> Self
    where
        F: Fn(&str) -> Option<PathBuf>,
    {
        let home = lookup("HOME")
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp")));
        let assets =
            lookup("AOOSTAR_ASSET_DIR").unwrap_or_else(|| PathBuf::from("/usr/share/aoostar-rs"));
        let binaries =
            lookup("AOOSTAR_BIN_DIR").unwrap_or_else(|| PathBuf::from("/usr/lib/aoostar-rs"));
        let config = lookup("AOOSTAR_CONFIG_DIR")
            .or_else(|| lookup("XDG_CONFIG_HOME").map(|p| p.join("aoostar-rs")))
            .unwrap_or_else(|| home.join(".config/aoostar-rs"));
        let state = lookup("AOOSTAR_STATE_DIR")
            .or_else(|| lookup("XDG_STATE_HOME").map(|p| p.join("aoostar-rs")))
            .unwrap_or_else(|| home.join(".local/state/aoostar-rs"));
        let runtime = lookup("AOOSTAR_RUNTIME_DIR")
            .or_else(|| lookup("XDG_RUNTIME_DIR").map(|p| p.join("aoostar-rs")))
            .unwrap_or_else(|| state.join("runtime"));
        Self {
            logs: state.join("logs"),
            lock: runtime.join("launcher.lock"),
            display_state: runtime.join("display.state"),
            stuck_file: runtime.join("uart.stuck"),
            sensors: runtime.join("sensors"),
            assets,
            binaries,
            config,
            state,
            runtime,
        }
    }

    pub fn assets_cfg(&self) -> PathBuf {
        self.assets.join("cfg")
    }
    pub fn fonts(&self) -> PathBuf {
        self.assets.join("fonts")
    }
    pub fn launcher_config(&self) -> PathBuf {
        self.config.join("launcher.toml")
    }
    pub fn launcher_log(&self) -> PathBuf {
        self.logs.join("launcher.log")
    }
    pub fn monitor_config(&self, name: &str) -> PathBuf {
        let p = PathBuf::from(name);
        if p.is_absolute() {
            p
        } else {
            self.assets_cfg().join(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_are_user_writable_and_absolute_by_default() {
        let p = LauncherPaths::from_lookup(|_| None);
        assert!(p.config.ends_with("aoostar-rs"));
        assert!(p.runtime.ends_with("aoostar-rs") || p.runtime.ends_with("runtime"));
        assert!(p.sensors.starts_with(&p.runtime));
        assert!(p.config.is_absolute());
        assert!(p.runtime.is_absolute());
    }

    #[test]
    fn xdg_and_aoostar_overrides_have_expected_precedence() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("HOME", PathBuf::from("/home/tester"));
        vars.insert("XDG_CONFIG_HOME", PathBuf::from("/tmp/config"));
        vars.insert("XDG_STATE_HOME", PathBuf::from("/tmp/state"));
        vars.insert("XDG_RUNTIME_DIR", PathBuf::from("/run/user/42"));
        vars.insert("AOOSTAR_CONFIG_DIR", PathBuf::from("/tmp/custom-config"));
        let p = LauncherPaths::from_lookup(|name| vars.get(name).cloned());
        assert_eq!(p.config, PathBuf::from("/tmp/custom-config"));
        assert_eq!(p.logs, PathBuf::from("/tmp/state/aoostar-rs/logs"));
        assert_eq!(p.runtime, PathBuf::from("/run/user/42/aoostar-rs"));
    }
    #[test]
    fn monitor_absolute_path_is_preserved() {
        let p = LauncherPaths::linux();
        let absolute = if cfg!(windows) {
            PathBuf::from(r"C:\monitor.json")
        } else {
            PathBuf::from("/tmp/monitor.json")
        };
        assert_eq!(p.monitor_config(absolute.to_str().unwrap()), absolute);
    }
}
