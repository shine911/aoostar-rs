// SPDX-License-Identifier: MIT OR Apache-2.0

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

#[cfg(windows)]
#[derive(Clone)]
pub struct ChildHandle {
    pub name: &'static str,
    pub healthy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub current_child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
}

/// Force-kills every currently running child (used on power suspend and on
/// launcher shutdown). Safe to call with children already dead.
#[cfg(windows)]
pub(crate) fn kill_all(handles: &[ChildHandle]) {
    for handle in handles {
        if let Ok(mut guard) = handle.current_child.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Base delay between a child exiting (or failing to start) and the next
/// spawn attempt.
#[cfg(windows)]
const RETRY_DELAY_SECS: u64 = 2;

/// Number of consecutive *spawn failures* tolerated at the base delay before
/// the delay starts widening. A child that can never start (missing exe,
/// blocked by policy, ...) would otherwise be retried — and logged — every
/// 2 seconds forever.
#[cfg(windows)]
const FAILURE_BACKOFF_THRESHOLD: u32 = 3;

/// Upper bound on the widened retry delay.
#[cfg(windows)]
const MAX_BACKOFF_SECS: u64 = 60;

/// Retry delay for the given number of consecutive spawn failures: the base
/// delay up to the threshold, then doubling, capped at [`MAX_BACKOFF_SECS`].
#[cfg(windows)]
fn retry_delay(consecutive_failures: u32) -> std::time::Duration {
    let secs = if consecutive_failures > FAILURE_BACKOFF_THRESHOLD {
        let shift = (consecutive_failures - FAILURE_BACKOFF_THRESHOLD).min(5);
        (RETRY_DELAY_SECS << shift).min(MAX_BACKOFF_SECS)
    } else {
        RETRY_DELAY_SECS
    };
    std::time::Duration::from_secs(secs)
}

/// Sleeps for up to `total`, waking early as soon as `quit` is set. Used for
/// the restart/backoff delays so shutdown never has to wait out a delay
/// (up to [`MAX_BACKOFF_SECS`]) before the watcher thread can be joined.
#[cfg(windows)]
fn sleep_until_quit(quit: &std::sync::atomic::AtomicBool, total: std::time::Duration) {
    use std::sync::atomic::Ordering;

    const STEP: std::time::Duration = std::time::Duration::from_millis(200);
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if quit.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(STEP);
    }
}

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
/// current child for a forced kill, plus the watcher thread's `JoinHandle`
/// so shutdown can wait for the thread to actually observe `quit` (a
/// watcher still running while the process is torn down could orphan a
/// hidden elevated child).
#[cfg(windows)]
pub fn spawn_and_watch(
    spec: ChildSpec,
    quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
    suspended: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (ChildHandle, std::thread::JoinHandle<()>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let healthy = Arc::new(AtomicBool::new(false));
    let current_child: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));

    let handle = ChildHandle {
        name: spec.name,
        healthy: healthy.clone(),
        current_child: current_child.clone(),
    };

    let watcher = std::thread::spawn(move || {
        // Truncate this child's log once per launcher session, so a log
        // represents "this run" instead of growing without bound across
        // every run ever. `spawn_child`'s append-mode opens (and the
        // restart markers written below) then accumulate within the
        // session, including across restarts.
        if let Some(parent) = spec.log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::File::create(&spec.log_path);

        let mut consecutive_failures: u32 = 0;

        loop {
            if quit.load(Ordering::SeqCst) {
                break;
            }

            // While the machine is asleep, don't spawn or respawn anything:
            // the power monitor killed the children on suspend and will
            // clear this flag after wake, at which point we start fresh.
            if suspended.load(Ordering::SeqCst) {
                sleep_until_quit(&quit, std::time::Duration::from_millis(250));
                continue;
            }

            match spawn_child(&spec) {
                Ok(child) => {
                    consecutive_failures = 0;
                    healthy.store(true, Ordering::SeqCst);
                    *current_child.lock().unwrap() = Some(child);

                    // Re-check quit (and the suspend flag) immediately after
                    // storing the child: a shutdown's or the power monitor's
                    // kill attempt that ran while spawn_child() was doing I/O
                    // (before the child existed to be killed) would have
                    // no-op'd on a still-None current_child. This re-check
                    // guarantees we self-kill instead of leaking an orphaned
                    // hidden process — or one that would hold the serial port
                    // through a sleep. On quit we stop the watcher; on suspend
                    // we loop back and wait for the flag to clear.
                    if quit.load(Ordering::SeqCst) || suspended.load(Ordering::SeqCst) {
                        if let Some(child) = current_child.lock().unwrap().as_mut() {
                            let _ = child.kill();
                        }
                        if quit.load(Ordering::SeqCst) {
                            break;
                        }
                    }

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
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    let delay = retry_delay(consecutive_failures);
                    crate::logging::append_line(
                        &spec.log_path,
                        &format!(
                            "failed to start {}: {err} (attempt {consecutive_failures}, retrying in {}s)",
                            spec.name,
                            delay.as_secs()
                        ),
                    );
                }
            }

            sleep_until_quit(&quit, retry_delay(consecutive_failures));
        }
    });

    (handle, watcher)
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
