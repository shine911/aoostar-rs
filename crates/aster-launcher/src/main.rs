// SPDX-License-Identifier: MIT OR Apache-2.0

#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod device;
mod logging;
mod power;
mod process;
#[cfg(windows)]
mod tray;

fn main() {
    #[cfg(windows)]
    windows_main();

    #[cfg(not(windows))]
    {
        eprintln!("aster-launcher is Windows-only.");
        std::process::exit(1);
    }
}

/// `ERROR_SHARING_VIOLATION` / `ERROR_LOCK_VIOLATION`: what opening the lock
/// file fails with while another live instance holds it.
#[cfg(windows)]
const SHARING_VIOLATION_CODES: [i32; 2] = [32, 33];

/// Opens `path` as this process's single-instance lock. The returned handle
/// must be kept alive for as long as the lock should be held.
///
/// Uses a share mode of 0 (deny all sharing) rather than `create_new`:
/// `create_new` keys off the *existence* of the file, so a lock file left
/// behind by a crashed instance would lock out every future launch until
/// someone deleted it by hand. Denying sharing instead keys off the *open
/// handle*, which Windows releases automatically when the owning process
/// exits for any reason (clean exit, panic, kill, crash) — so a stale lock
/// file is reclaimed on the next launch, while a genuine second instance
/// fails with `ERROR_SHARING_VIOLATION`.
#[cfg(windows)]
fn acquire_instance_lock(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(path)?;

    // Best-effort diagnostics only; the lock is the handle, not the contents.
    let _ = write!(file, "{}", std::process::id());
    Ok(file)
}

#[cfg(windows)]
fn windows_main() {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

    let base_dir = std::env::current_exe()
        .expect("cannot resolve current exe path")
        .parent()
        .expect("exe has no parent directory")
        .to_path_buf();

    std::fs::create_dir_all(base_dir.join("logs")).ok();
    let launcher_log = base_dir.join("logs").join("launcher.log");

    // Single-instance guard: two launchers would fight over the same serial
    // port, the same sensor files and the same logs (an accidental
    // double-click is easy — nothing is visible for the first second or
    // two). The handle is held (via `_instance_lock`) for the rest of this
    // function, so the lock lives exactly as long as this process does.
    let _instance_lock = match acquire_instance_lock(&base_dir.join(".launcher.lock")) {
        Ok(file) => Some(file),
        Err(err) if SHARING_VIOLATION_CODES.contains(&err.raw_os_error().unwrap_or(0)) => {
            logging::append_line(
                &launcher_log,
                "another aster-launcher instance already holds .launcher.lock — exiting without starting children",
            );
            return;
        }
        // Anything else (odd ACL on base_dir, read-only media, ...) is not
        // evidence of a second instance, so don't refuse to start over it —
        // just note that this run is unguarded.
        Err(err) => {
            logging::append_line(
                &launcher_log,
                &format!(
                    "could not create .launcher.lock ({err}) — continuing without single-instance protection"
                ),
            );
            None
        }
    };

    let cfg = config::LauncherConfig::load(&base_dir.join("launcher.toml"));
    let config_path = base_dir.join("launcher.toml");
    // Shared, mutable child specs: the tray's refresh menu rewrites them and
    // restarts the children, whose watchers re-read the specs on every spawn.
    let specs: Arc<Mutex<[process::ChildSpec; 3]>> =
        Arc::new(Mutex::new(process::child_specs(&base_dir, &cfg)));
    let current_refresh = Arc::new(AtomicU16::new(cfg.sysinfo_refresh_effective()));
    // Active theme for the tray "Themes" check mark; Default (0) is the
    // effective fallback when launcher.toml does not configure a theme.
    let current_theme = Arc::new(AtomicU16::new(cfg.theme.unwrap_or(0)));

    let quit = Arc::new(AtomicBool::new(false));
    let suspended = Arc::new(AtomicBool::new(false));
    let mut handles: Vec<process::ChildHandle> = Vec::with_capacity(3);
    let mut watchers: Vec<std::thread::JoinHandle<()>> = Vec::with_capacity(3);
    for index in 0..3 {
        let (handle, watcher) =
            process::spawn_and_watch(index, specs.clone(), quit.clone(), suspended.clone());
        handles.push(handle);
        watchers.push(watcher);
    }

    // Power monitor: kills children on sleep, respawns them after wake.
    // The thread is a daemon (see `power::start`) and is never joined.
    let _power_thread = power::start(
        suspended.clone(),
        Arc::new(handles.clone()),
        cfg.restart_uart_on_resume,
        launcher_log.clone(),
    );

    tray::run(
        &handles,
        specs,
        current_refresh,
        current_theme,
        quit.clone(),
        &launcher_log,
        &config_path,
        &base_dir,
    );

    // tray::run returned because quit was set (Quit clicked) or because
    // the tray icon could not be created at all — make sure every watcher
    // thread stops trying to restart, then force-kill whichever child is
    // currently running.
    quit.store(true, Ordering::SeqCst);
    process::kill_all(&handles);

    // Then wait for each watcher thread to actually observe `quit` and exit.
    // Without this, `main` could return — tearing the process down — while a
    // watcher is mid-spawn, leaving a hidden elevated child orphaned. The
    // watcher loop breaks on `quit` at every step (including inside its
    // restart/backoff delays), so these joins return promptly.
    for watcher in watchers {
        let _ = watcher.join();
    }
}
