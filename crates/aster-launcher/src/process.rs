use crate::config::LauncherConfig;
use std::path::{Path, PathBuf};

pub struct ChildSpec {
    pub name: &'static str,
    pub base_dir: PathBuf,
    pub exe_path: PathBuf,
    pub args: Vec<String>,
    pub log_path: PathBuf,
}

/// Builds the 3 child process specs, all paths relative to `base_dir` (the
/// launcher's own exe directory) so a copied/zipped `dist/` folder keeps
/// working wherever it's placed.
pub fn child_specs(base_dir: &Path, cfg: &LauncherConfig) -> [ChildSpec; 3] {
    let logs_dir = base_dir.join("logs");
    [
        ChildSpec {
            name: "aster-sysinfo",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("bin").join("aster-sysinfo.exe"),
            args: vec![
                "--out".to_string(),
                "cfg\\sensors\\sysinfo.txt".to_string(),
                "--temp-dir".to_string(),
                "cfg\\sensors".to_string(),
                "--refresh".to_string(),
                cfg.sysinfo_refresh.to_string(),
            ],
            log_path: logs_dir.join("aster-sysinfo.log"),
        },
        ChildSpec {
            name: "asterctl",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("bin").join("asterctl.exe"),
            args: vec!["--config".to_string(), cfg.monitor_config.clone()],
            log_path: logs_dir.join("asterctl.log"),
        },
        ChildSpec {
            name: "hwbridge",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("hwbridge").join("HwBridge.exe"),
            args: vec![
                "cfg\\sensors\\hwbridge.txt".to_string(),
                cfg.hwbridge_refresh.to_string(),
            ],
            log_path: logs_dir.join("hwbridge.log"),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_specs_relative_to_base_dir_using_config_values() {
        let base_dir = Path::new("C:\\dist");
        let cfg = LauncherConfig {
            monitor_config: "Custom.json".to_string(),
            sysinfo_refresh: 7,
            hwbridge_refresh: 11,
        };

        let specs = child_specs(base_dir, &cfg);

        assert_eq!(specs[0].name, "aster-sysinfo");
        assert_eq!(
            specs[0].exe_path,
            base_dir.join("bin").join("aster-sysinfo.exe")
        );
        assert_eq!(specs[0].args.last().unwrap(), "7");
        assert_eq!(
            specs[0].log_path,
            base_dir.join("logs").join("aster-sysinfo.log")
        );

        assert_eq!(specs[1].name, "asterctl");
        assert_eq!(specs[1].exe_path, base_dir.join("bin").join("asterctl.exe"));
        assert_eq!(
            specs[1].args,
            vec!["--config".to_string(), "Custom.json".to_string()]
        );

        assert_eq!(specs[2].name, "hwbridge");
        assert_eq!(
            specs[2].exe_path,
            base_dir.join("hwbridge").join("HwBridge.exe")
        );
        assert_eq!(
            specs[2].args,
            vec!["cfg\\sensors\\hwbridge.txt".to_string(), "11".to_string()]
        );

        for spec in &specs {
            assert_eq!(spec.base_dir, base_dir);
        }
    }
}
