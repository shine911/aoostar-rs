// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::config::LauncherConfig;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct ChildSpec {
    pub name: &'static str,
    pub base_dir: PathBuf,
    pub exe_path: PathBuf,
    pub args: Vec<String>,
    pub log_path: PathBuf,
}

/// Builds the platform child process specs. Windows keeps its portable
/// `dist/` layout; Linux uses the XDG-aware resolver and starts two children.
#[cfg(windows)]
pub fn child_specs(base_dir: &Path, cfg: &LauncherConfig) -> Vec<ChildSpec> {
    let logs_dir = base_dir.join("logs");
    vec![
        ChildSpec {
            name: "aster-sysinfo",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("bin").join("aster-sysinfo.exe"),
            args: vec![
                "--shm".to_string(),
                "--refresh".to_string(),
                cfg.sysinfo_refresh_effective().to_string(),
            ],
            log_path: logs_dir.join("aster-sysinfo.log"),
        },
        ChildSpec {
            name: "asterctl",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("bin").join("asterctl.exe"),
            args: {
                let mut args = vec![
                    "--config".to_string(),
                    cfg.monitor_config.clone(),
                    "--shm".to_string(),
                    "--display-state".to_string(),
                    base_dir
                        .join("cfg")
                        .join("display.state")
                        .to_string_lossy()
                        .into_owned(),
                    "--stuck-file".to_string(),
                    base_dir
                        .join("cfg")
                        .join("uart.stuck")
                        .to_string_lossy()
                        .into_owned(),
                ];
                if let Some(theme) = cfg.theme {
                    args.push("--theme".to_string());
                    args.push(theme.to_string());
                }
                args
            },
            log_path: logs_dir.join("asterctl.log"),
        },
        ChildSpec {
            name: "hwbridge",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("hwbridge").join("HwBridge.exe"),
            args: vec![
                "--shm".to_string(),
                cfg.hwbridge_refresh_effective().to_string(),
            ],
            log_path: logs_dir.join("hwbridge.log"),
        },
    ]
}

#[cfg(target_os = "linux")]
pub fn linux_child_specs(
    paths: &crate::paths::LauncherPaths,
    cfg: &LauncherConfig,
) -> Vec<ChildSpec> {
    let mut ctl_args = vec![
        "--config".into(),
        paths
            .monitor_config(&cfg.monitor_config)
            .to_string_lossy()
            .into_owned(),
        "--config-dir".into(),
        paths.assets_cfg().to_string_lossy().into_owned(),
        "--font-dir".into(),
        paths.fonts().to_string_lossy().into_owned(),
        "--sensor-mapping".into(),
        paths
            .assets_cfg()
            .join("sensor-mapping-linux.cfg")
            .to_string_lossy()
            .into_owned(),
        "--sensor-path".into(),
        paths.sensors.to_string_lossy().into_owned(),
        "--shm".into(),
        "--display-state".into(),
        paths.display_state.to_string_lossy().into_owned(),
        "--stuck-file".into(),
        paths.stuck_file.to_string_lossy().into_owned(),
    ];
    if let Some(theme) = cfg.theme {
        ctl_args.extend(["--theme".into(), theme.to_string()]);
    }
    vec![
        ChildSpec {
            name: "aster-sysinfo",
            base_dir: paths.assets.clone(),
            exe_path: paths.binaries.join("aster-sysinfo"),
            args: vec![
                "--shm".into(),
                "--refresh".into(),
                cfg.sysinfo_refresh_effective().to_string(),
            ],
            log_path: paths.logs.join("aster-sysinfo.log"),
        },
        ChildSpec {
            name: "asterctl",
            base_dir: paths.assets.clone(),
            exe_path: paths.binaries.join("asterctl"),
            args: ctl_args,
            log_path: paths.logs.join("asterctl.log"),
        },
    ]
}

#[derive(Clone)]
pub struct ChildHandle {
    pub name: &'static str,
    pub healthy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub current_child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    /// Set (under the `current_child` lock, only while a child exists) by
    /// every deliberate kill — tray restarts, suspend, stuck-marker rescan,
    /// shutdown — and consumed by the watcher when that child exits, so a
    /// kill is not mistaken for a crash: `TerminateProcess` makes the child
    /// exit with code 1, which within [`QUICK_EXIT_THRESHOLD`] of its spawn
    /// would otherwise grow the restart backoff.
    pub killed_on_purpose: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Locks `mutex`, recovering the data if a previous holder panicked. The
/// guarded values here (child specs, the current `Child`) stay valid after
/// a panic elsewhere, and a watcher must keep supervising — and be able to
/// kill — its child instead of dying on a poisoned lock (never-panic
/// contract).
pub(crate) fn lock_recover<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Kills `child` as a deliberate act (see [`ChildHandle::killed_on_purpose`]).
/// Must be called with the `current_child` lock held.
fn kill_on_purpose(
    child: &mut std::process::Child,
    killed_on_purpose: &std::sync::atomic::AtomicBool,
) -> std::io::Result<()> {
    killed_on_purpose.store(true, std::sync::atomic::Ordering::SeqCst);
    child.kill()
}

/// Best-effort force-kill of every currently running child (used on power
/// suspend and on launcher shutdown to stop the children immediately). This
/// is NOT the guarantee that children die: it can miss a child that is
/// being respawned at that moment (`current_child` still `None`), and a
/// failed `TerminateProcess` used to be swallowed silently — both are why
/// the watcher threads force-kill their own children on quit/suspend too
/// (see `spawn_and_watch`). Safe to call with children already dead.
pub(crate) fn kill_all(handles: &[ChildHandle], log_path: &Path) {
    for handle in handles {
        if let Some(child) = lock_recover(&handle.current_child).as_mut()
            && let Err(err) = kill_on_purpose(child, &handle.killed_on_purpose)
        {
            crate::logging::append_line(
                log_path,
                &format!("kill_all: failed to kill {}: {err}", handle.name),
            );
        }
    }
}

/// Force-kills only the children whose names match `names` (used by the
/// tray's refresh menu to restart exactly the refresh-driven processes;
/// their watchers respawn them with the updated specs). Safe to call with
/// children already dead.
pub(crate) fn kill_named(handles: &[ChildHandle], names: &[&str]) -> Result<(), String> {
    let mut errors = Vec::new();
    for handle in handles {
        if !names.contains(&handle.name) {
            continue;
        }
        if let Some(child) = lock_recover(&handle.current_child).as_mut()
            && let Err(err) = kill_on_purpose(child, &handle.killed_on_purpose)
        {
            errors.push(format!("failed to restart {}: {err}", handle.name));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Base delay between a child exiting (or failing to start) and the next
/// spawn attempt.
const RETRY_DELAY_SECS: u64 = 2;

/// Grace period the watchers wait (after the launcher signals its shutdown
/// sequence has started, see `shutting_down` in `spawn_and_watch`) before
/// force-killing their children on a clean quit. It lets asterctl poll the
/// "off" display-state once (it re-polls every ~1s, see `STATE_POLL_STEP`
/// in asterctl) so the LCD is blanked before the processes die. main.rs
/// uses the same value for its own pre-kill sleep.
pub(crate) const QUIT_KILL_GRACE_SECS: u64 = 2;

/// Number of consecutive *spawn failures* tolerated at the base delay before
/// the delay starts widening. A child that can never start (missing exe,
/// blocked by policy, ...) would otherwise be retried — and logged — every
/// 2 seconds forever.
const FAILURE_BACKOFF_THRESHOLD: u32 = 3;

/// Upper bound on the widened retry delay.
const MAX_BACKOFF_SECS: u64 = 60;

/// How long a child may run and still count as a "quick" exit for backoff
/// purposes. An asterctl that crashes on the LCD display init after resume
/// exits within ~3s; a healthy child runs for minutes. Kept cross-platform
/// so the decision logic is unit-testable on any host.
const QUICK_EXIT_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(10);

/// True if a child that just exited should count as a backoff-worthy
/// failure: it exited with a non-success status shortly after spawn
/// (e.g. asterctl failing the LCD display init after resume). Such
/// crash-exit loops would otherwise be restarted at the base delay forever,
/// hammering the serial port every few seconds.
///
/// Kept cross-platform (and `#[allow(dead_code)]` on non-Windows builds)
/// so the logic is covered by the cross-platform unit tests.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_quick_failure(status: Option<std::process::ExitStatus>, uptime: std::time::Duration) -> bool {
    match status {
        Some(status) if !status.success() => uptime < QUICK_EXIT_THRESHOLD,
        _ => false,
    }
}

/// Next value of a watcher's consecutive-failure counter after its child
/// exited: grows on a quick failing exit (see [`is_quick_failure`]), resets
/// otherwise — and always resets when the launcher itself killed the child
/// (`killed_on_purpose`: tray restart, suspend, stuck-marker rescan), since
/// a deliberate `TerminateProcess` exits with code 1 and would otherwise
/// look like a crash right after spawn.
fn next_consecutive_failures(
    previous: u32,
    status: Option<std::process::ExitStatus>,
    uptime: std::time::Duration,
    killed_on_purpose: bool,
) -> u32 {
    if !killed_on_purpose && is_quick_failure(status, uptime) {
        previous.saturating_add(1)
    } else {
        0
    }
}

/// How often a watcher checks its running child (`try_wait`) and the
/// quit/suspend flags. 1s keeps the 24/7 launcher at ~1 wakeup/s per
/// watcher (it was ~3-5/s) while staying well inside the shutdown budget:
/// the clean-quit path already waits [`QUIT_KILL_GRACE_SECS`] and suspend
/// kills the children directly via `kill_all`.
const WATCH_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Retry delay for the given number of consecutive spawn failures: the base
/// delay up to the threshold, then doubling, capped at [`MAX_BACKOFF_SECS`].
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
fn sleep_until_quit(quit: &std::sync::atomic::AtomicBool, total: std::time::Duration) {
    use std::sync::atomic::Ordering;

    let deadline = std::time::Instant::now() + total;
    loop {
        if quit.load(Ordering::SeqCst) {
            return;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        std::thread::sleep(WATCH_POLL.min(remaining));
    }
}

/// Rotates a child's log to `<log>.1` when it has grown past
/// [`crate::logging::MAX_LOG_BYTES`]. Called before every spawn: the log is
/// truncated only once per launcher session (see `spawn_and_watch`), so a
/// child that keeps crashing — or a launcher that runs for weeks — would
/// otherwise append to it without bound. Best effort; never fails a spawn.
fn rotate_child_log(log_path: &Path) {
    crate::logging::rotate_if_larger(log_path, crate::logging::MAX_LOG_BYTES);
}

#[cfg(windows)]
fn spawn_child(spec: &ChildSpec) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;

    rotate_child_log(&spec.log_path);
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

#[cfg(unix)]
fn spawn_child(spec: &ChildSpec) -> std::io::Result<std::process::Child> {
    rotate_child_log(&spec.log_path);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)?;
    let log_file_err = log_file.try_clone()?;
    std::process::Command::new(&spec.exe_path)
        .args(&spec.args)
        .current_dir(&spec.base_dir)
        .stdout(log_file)
        .stderr(log_file_err)
        .spawn()
}

/// Spawns the child at `index` of the shared `specs` in a background thread
/// that keeps it running: on unexpected exit it logs a restart marker and
/// relaunches, until `quit` is set. The specs are read fresh on every spawn
/// (not captured once), so updating `specs` and killing the current child —
/// as the tray's refresh menu does — restarts it with the new arguments.
/// Returns immediately with a handle to observe health / reach the current
/// child for a forced kill, plus the watcher thread's `JoinHandle` so
/// shutdown can wait for the thread to actually observe `quit` (a watcher
/// still running while the process is torn down could orphan a hidden
/// elevated child).
///
/// The watcher is also the *guarantee* that a spawned child never outlives
/// the launcher: on `quit` or `suspended` it force-kills its own child and
/// retries until the process is actually gone (see the wait loop). `kill_all`
/// is only a best-effort early kill — it can race a respawn and swallows
/// TerminateProcess errors — which is exactly how a child used to survive
/// shutdown (e.g. HwBridge.exe after Quit). `shutting_down` anchors the
/// clean-quit grace so the force-kill cannot preempt asterctl blanking the
/// LCD (see [`QUIT_KILL_GRACE_SECS`]).
pub fn spawn_and_watch(
    index: usize,
    specs: std::sync::Arc<std::sync::Mutex<Vec<ChildSpec>>>,
    quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
    suspended: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shutting_down: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (ChildHandle, std::thread::JoinHandle<()>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let healthy = Arc::new(AtomicBool::new(false));
    let current_child: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
    let killed_on_purpose = Arc::new(AtomicBool::new(false));

    // Callers iterate over the specs they built, so `index` is always
    // valid; still, never panic over it — report an inert "unknown" child.
    let Some(spec) = lock_recover(&specs).get(index).cloned() else {
        let handle = ChildHandle {
            name: "unknown",
            healthy,
            current_child,
            killed_on_purpose,
        };
        return (handle, std::thread::spawn(|| {}));
    };
    let name = spec.name;
    let log_path = spec.log_path.clone();

    let handle = ChildHandle {
        name,
        healthy: healthy.clone(),
        current_child: current_child.clone(),
        killed_on_purpose: killed_on_purpose.clone(),
    };

    let watcher = std::thread::spawn(move || {
        // Truncate this child's log once per launcher session, so a log
        // represents "this run" instead of growing without bound across
        // every run ever. `spawn_child`'s append-mode opens (and the
        // restart markers written below) then accumulate within the
        // session, including across restarts.
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::File::create(&log_path);

        let mut consecutive_failures: u32 = 0;

        loop {
            if quit.load(Ordering::SeqCst) {
                break;
            }

            // While the machine is asleep, don't spawn or respawn anything:
            // the power monitor killed the children on suspend and will
            // clear this flag after wake, at which point we start fresh.
            if suspended.load(Ordering::SeqCst) {
                sleep_until_quit(&quit, WATCH_POLL);
                continue;
            }

            // Read the spec fresh each spawn so a tray refresh change is
            // picked up on the next restart. (The specs vector is only ever
            // replaced wholesale with the same layout, so `index` stays
            // valid; the fallback just keeps the never-panic contract.)
            let Some(spec) = lock_recover(&specs).get(index).cloned() else {
                sleep_until_quit(&quit, retry_delay(consecutive_failures));
                continue;
            };

            match spawn_child(&spec) {
                Ok(child) => {
                    let spawn_time = std::time::Instant::now();
                    healthy.store(true, Ordering::SeqCst);
                    // Clear a stale flag (a kill that raced the previous
                    // child's exit) before the new child becomes reachable.
                    killed_on_purpose.store(false, Ordering::SeqCst);
                    *lock_recover(&current_child) = Some(child);

                    // Block until the child exits — or, on quit/suspend,
                    // until it has been force-killed (see below). The lock
                    // is held only briefly per check so a kill from another
                    // thread can still reach the child.
                    let mut grace_done = false;
                    let mut kill_notice_logged = false;
                    let status = loop {
                        // On a clean quit, first wait for the launcher's
                        // shutdown sequence to start (display-state write +
                        // grace for asterctl to blank the LCD), so this
                        // force-kill can never preempt the display-off.
                        // Anchored on `shutting_down` rather than `quit`
                        // because the tray loop may notice `quit` up to ~2s
                        // late.
                        if quit.load(Ordering::SeqCst) && !grace_done {
                            while !shutting_down.load(Ordering::SeqCst) {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                            std::thread::sleep(std::time::Duration::from_secs(
                                QUIT_KILL_GRACE_SECS,
                            ));
                            grace_done = true;
                        }

                        let mut guard = lock_recover(&current_child);
                        match guard.as_mut() {
                            Some(child) => {
                                if quit.load(Ordering::SeqCst) || suspended.load(Ordering::SeqCst) {
                                    // Shutdown or sleep: the child must not
                                    // outlive the launcher. `kill_all` is
                                    // best-effort (respawn race, swallowed
                                    // TerminateProcess errors), so the
                                    // watcher — which owns the Child —
                                    // force-kills it here and retries until
                                    // it is actually gone.
                                    match child.try_wait() {
                                        Ok(Some(status)) => break Some(status),
                                        _ => {
                                            let result = kill_on_purpose(child, &killed_on_purpose);
                                            if !kill_notice_logged {
                                                match result {
                                                    Ok(()) => crate::logging::append_line(
                                                        &log_path,
                                                        &format!(
                                                            "{name}: force-killed by watcher (kill_all missed it)"
                                                        ),
                                                    ),
                                                    Err(err) => crate::logging::append_line(
                                                        &log_path,
                                                        &format!(
                                                            "{name}: kill failed during shutdown/suspend: {err}; retrying"
                                                        ),
                                                    ),
                                                }
                                                kill_notice_logged = true;
                                            }
                                        }
                                    }
                                } else {
                                    match child.try_wait() {
                                        Ok(Some(status)) => break Some(status),
                                        Ok(None) => {}
                                        Err(_) => break None,
                                    }
                                }
                            }
                            None => break None,
                        }
                        drop(guard);
                        std::thread::sleep(WATCH_POLL);
                    };

                    *lock_recover(&current_child) = None;
                    healthy.store(false, Ordering::SeqCst);
                    let deliberate = killed_on_purpose.swap(false, Ordering::SeqCst);

                    if quit.load(Ordering::SeqCst) {
                        break;
                    }

                    // Grow the backoff when the child keeps exiting quickly
                    // with a failure (e.g. asterctl failing the LCD display
                    // init after resume): a crash-exit loop must not be
                    // restarted at the base delay forever, hammering the
                    // serial port every few seconds. A child that ran past
                    // QUICK_EXIT_THRESHOLD, exited cleanly, or was killed
                    // deliberately (tray menus, suspend, stuck-marker
                    // rescan) resets the counter so a normal restart stays
                    // immediate.
                    consecutive_failures = next_consecutive_failures(
                        consecutive_failures,
                        status,
                        spawn_time.elapsed(),
                        deliberate,
                    );

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
    use std::time::Duration;

    #[cfg(windows)]
    #[test]
    fn builds_specs_relative_to_base_dir_using_config_values() {
        let base_dir = Path::new("C:\\dist");
        let cfg = LauncherConfig {
            monitor_config: "Custom.json".to_string(),
            refresh_time: Some(10),
            sysinfo_refresh: Some(7),
            hwbridge_refresh: Some(11),
            restart_uart_on_resume: true,
            theme: None,
            display_mode: None,
        };

        let specs = child_specs(base_dir, &cfg);

        assert_eq!(specs[0].name, "aster-sysinfo");
        assert_eq!(
            specs[0].exe_path,
            base_dir.join("bin").join("aster-sysinfo.exe")
        );
        // the shared refresh_time wins over the legacy per-process keys
        assert_eq!(
            specs[0].args,
            vec![
                "--shm".to_string(),
                "--refresh".to_string(),
                "10".to_string()
            ]
        );
        assert_eq!(
            specs[0].log_path,
            base_dir.join("logs").join("aster-sysinfo.log")
        );

        assert_eq!(specs[1].name, "asterctl");
        assert_eq!(specs[1].exe_path, base_dir.join("bin").join("asterctl.exe"));
        assert_eq!(
            specs[1].args,
            vec![
                "--config".to_string(),
                "Custom.json".to_string(),
                "--shm".to_string(),
                "--display-state".to_string(),
                base_dir
                    .join("cfg")
                    .join("display.state")
                    .to_string_lossy()
                    .into_owned(),
                "--stuck-file".to_string(),
                base_dir
                    .join("cfg")
                    .join("uart.stuck")
                    .to_string_lossy()
                    .into_owned(),
            ]
        );

        assert_eq!(specs[2].name, "hwbridge");
        assert_eq!(
            specs[2].exe_path,
            base_dir.join("hwbridge").join("HwBridge.exe")
        );
        assert_eq!(specs[2].args, vec!["--shm".to_string(), "10".to_string()]);

        for spec in &specs {
            assert_eq!(spec.base_dir, base_dir);
        }
    }

    #[cfg(windows)]
    #[test]
    fn legacy_refresh_values_flow_through_when_no_shared_refresh_time() {
        let base_dir = Path::new("C:\\dist");
        let cfg = LauncherConfig {
            monitor_config: "Custom.json".to_string(),
            refresh_time: None,
            sysinfo_refresh: Some(2),
            hwbridge_refresh: Some(30),
            restart_uart_on_resume: true,
            theme: None,
            display_mode: None,
        };

        let specs = child_specs(base_dir, &cfg);

        assert_eq!(specs[0].args.last().unwrap(), "2");
        assert_eq!(specs[2].args, vec!["--shm".to_string(), "30".to_string()]);
    }

    #[cfg(windows)]
    #[test]
    fn theme_flag_flows_through_to_asterctl_args() {
        let base_dir = Path::new("C:\\dist");
        let cfg = LauncherConfig {
            monitor_config: "Monitor3.json".to_string(),
            refresh_time: None,
            sysinfo_refresh: None,
            hwbridge_refresh: None,
            restart_uart_on_resume: true,
            theme: Some(2),
            display_mode: None,
        };

        let specs = child_specs(base_dir, &cfg);

        assert_eq!(
            specs[1].args,
            vec![
                "--config".to_string(),
                "Monitor3.json".to_string(),
                "--shm".to_string(),
                "--display-state".to_string(),
                base_dir
                    .join("cfg")
                    .join("display.state")
                    .to_string_lossy()
                    .into_owned(),
                "--stuck-file".to_string(),
                base_dir
                    .join("cfg")
                    .join("uart.stuck")
                    .to_string_lossy()
                    .into_owned(),
                "--theme".to_string(),
                "2".to_string()
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn refresh_and_theme_flow_through_together() {
        let base_dir = Path::new("C:\\dist");
        let cfg = LauncherConfig {
            monitor_config: "Monitor3.json".to_string(),
            refresh_time: Some(10),
            sysinfo_refresh: None,
            hwbridge_refresh: None,
            restart_uart_on_resume: true,
            theme: Some(2),
            display_mode: None,
        };

        let specs = child_specs(base_dir, &cfg);

        assert_eq!(specs[0].args.last().unwrap(), "10");
        assert_eq!(
            specs[1].args,
            vec![
                "--config".to_string(),
                "Monitor3.json".to_string(),
                "--shm".to_string(),
                "--display-state".to_string(),
                base_dir
                    .join("cfg")
                    .join("display.state")
                    .to_string_lossy()
                    .into_owned(),
                "--stuck-file".to_string(),
                base_dir
                    .join("cfg")
                    .join("uart.stuck")
                    .to_string_lossy()
                    .into_owned(),
                "--theme".to_string(),
                "2".to_string()
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_specs_have_two_children_and_absolute_runtime_arguments() {
        let paths = crate::paths::LauncherPaths {
            assets: PathBuf::from("/usr/share/aoostar-rs"),
            binaries: PathBuf::from("/usr/lib/aoostar-rs"),
            config: PathBuf::from("/tmp/c"),
            state: PathBuf::from("/tmp/s"),
            runtime: PathBuf::from("/tmp/r"),
            logs: PathBuf::from("/tmp/s/logs"),
            lock: PathBuf::from("/tmp/r/launcher.lock"),
            display_state: PathBuf::from("/tmp/r/display.state"),
            stuck_file: PathBuf::from("/tmp/r/uart.stuck"),
            sensors: PathBuf::from("/tmp/r/sensors"),
        };
        let specs = linux_child_specs(&paths, &LauncherConfig::default());
        assert_eq!(specs.len(), 2);
        assert!(specs.iter().all(|s| s.exe_path.is_absolute()));
        assert!(
            specs[1]
                .args
                .windows(2)
                .any(|w| w == ["--config-dir", "/usr/share/aoostar-rs/cfg"])
        );
        assert!(specs[1].args.windows(2).any(|w| w
            == [
                "--sensor-mapping",
                "/usr/share/aoostar-rs/cfg/sensor-mapping-linux.cfg"
            ]));
        assert!(!specs.iter().any(|s| s.name == "hwbridge"));
    }

    #[test]
    fn quick_exit_with_failure_counts_as_backoff_failure() {
        // asterctl crashing on the LCD display init after resume: exits
        // non-zero within ~3s of spawn.
        assert!(is_quick_failure(
            Some(exit_status_non_success()),
            Duration::from_secs(2)
        ));
        // A child that ran for a while before failing (or was killed by the
        // tray) must NOT grow the backoff.
        assert!(!is_quick_failure(
            Some(exit_status_non_success()),
            Duration::from_secs(30)
        ));
        // A clean exit never counts, however quick.
        assert!(!is_quick_failure(
            Some(exit_status_success()),
            Duration::from_secs(1)
        ));
        // No status (spawn-level error path) never counts either.
        assert!(!is_quick_failure(None, Duration::from_secs(1)));
    }

    #[test]
    fn deliberate_kill_resets_the_failure_counter() {
        // TerminateProcess → exit code 1 two seconds after spawn: a crash
        // by the numbers, but the launcher killed it (tray restart, ...).
        assert_eq!(
            next_consecutive_failures(
                3,
                Some(exit_status_non_success()),
                Duration::from_secs(2),
                true
            ),
            0
        );
        // The same exit without the flag is a crash and grows the backoff.
        assert_eq!(
            next_consecutive_failures(
                3,
                Some(exit_status_non_success()),
                Duration::from_secs(2),
                false
            ),
            4
        );
        // A long-running child resets the counter either way.
        assert_eq!(
            next_consecutive_failures(
                3,
                Some(exit_status_non_success()),
                Duration::from_secs(60),
                false
            ),
            0
        );
        assert_eq!(
            next_consecutive_failures(
                u32::MAX,
                Some(exit_status_non_success()),
                Duration::ZERO,
                false
            ),
            u32::MAX
        );
    }

    #[test]
    fn lock_recover_survives_a_poisoned_mutex() {
        let mutex = std::sync::Arc::new(std::sync::Mutex::new(vec![1, 2, 3]));
        let poisoner = mutex.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(mutex.is_poisoned());
        // The data is still usable: a watcher keeps working.
        assert_eq!(lock_recover(&mutex).get(1).copied(), Some(2));
    }

    // Unix-only: needs a long-running stand-in child (`sleep`).
    #[cfg(unix)]
    #[test]
    fn kill_named_marks_only_matching_running_children_as_deliberate() {
        let spawn = || {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap()
        };
        let handle = |name: &'static str, child: Option<std::process::Child>| ChildHandle {
            name,
            healthy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            current_child: std::sync::Arc::new(std::sync::Mutex::new(child)),
            killed_on_purpose: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let handles = vec![
            handle("aster-sysinfo", Some(spawn())),
            handle("asterctl", Some(spawn())),
            handle("hwbridge", None),
        ];

        kill_named(&handles, &["aster-sysinfo", "hwbridge"]).unwrap();

        let flag = |i: usize| {
            handles[i]
                .killed_on_purpose
                .load(std::sync::atomic::Ordering::SeqCst)
        };
        assert!(flag(0), "killed child is marked deliberate");
        assert!(!flag(1), "untouched child stays unmarked");
        assert!(!flag(2), "no child, nothing to mark");
        let status = lock_recover(&handles[0].current_child)
            .as_mut()
            .unwrap()
            .wait()
            .unwrap();
        assert!(!status.success());

        let dir = tempfile::tempdir().unwrap();
        kill_all(&handles, &dir.path().join("launcher.log"));
        assert!(flag(1));
        let _ = lock_recover(&handles[1].current_child)
            .as_mut()
            .unwrap()
            .wait();
    }

    #[cfg(unix)]
    fn exit_status_non_success() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }

    #[cfg(windows)]
    fn exit_status_non_success() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }

    #[cfg(unix)]
    fn exit_status_success() -> std::process::ExitStatus {
        std::process::Command::new("true").status().unwrap()
    }

    #[cfg(windows)]
    fn exit_status_success() -> std::process::ExitStatus {
        std::process::Command::new("cmd")
            .args(["/c", "exit", "0"])
            .status()
            .unwrap()
    }
}
