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

#[cfg(windows)]
pub struct ChildHandle {
    pub name: &'static str,
    pub healthy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub current_child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn spawn_child(spec: &ChildSpec) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)?;
    let log_file_err = log_file.try_clone()?;

    std::process::Command::new(&spec.exe_path)
        .args(&spec.args)
        .current_dir(&spec.base_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(log_file)
        .stderr(log_file_err)
        .spawn()
}

/// Spawns `spec` in a background thread that keeps it running: on
/// unexpected exit it logs a restart marker and relaunches, until `quit`
/// is set. Returns immediately with a handle to observe health / reach the
/// current child for a forced kill.
#[cfg(windows)]
pub fn spawn_and_watch(
    spec: ChildSpec,
    quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> ChildHandle {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let healthy = Arc::new(AtomicBool::new(false));
    let current_child: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));

    let handle = ChildHandle {
        name: spec.name,
        healthy: healthy.clone(),
        current_child: current_child.clone(),
    };

    std::thread::spawn(move || {
        loop {
            if quit.load(Ordering::SeqCst) {
                break;
            }

            match spawn_child(&spec) {
                Ok(child) => {
                    healthy.store(true, Ordering::SeqCst);
                    *current_child.lock().unwrap() = Some(child);

                    // Block until the child exits, without holding the lock
                    // (so a quit-triggered kill() from another thread can
                    // still reach it).
                    let status = loop {
                        let mut guard = current_child.lock().unwrap();
                        match guard.as_mut() {
                            Some(child) => match child.try_wait() {
                                Ok(Some(status)) => break Some(status),
                                Ok(None) => {}
                                Err(_) => break None,
                            },
                            None => break None,
                        }
                        drop(guard);
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    };

                    *current_child.lock().unwrap() = None;
                    healthy.store(false, Ordering::SeqCst);

                    if quit.load(Ordering::SeqCst) {
                        break;
                    }

                    crate::logging::append_line(
                        &spec.log_path,
                        &format!("--- {} exited ({status:?}), restarting ---", spec.name),
                    );
                }
                Err(err) => {
                    healthy.store(false, Ordering::SeqCst);
                    crate::logging::append_line(
                        &spec.log_path,
                        &format!("failed to start {}: {err}", spec.name),
                    );
                }
            }

            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });

    handle
}
