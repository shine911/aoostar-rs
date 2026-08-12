#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod logging;
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

#[cfg(windows)]
fn windows_main() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let base_dir = std::env::current_exe()
        .expect("cannot resolve current exe path")
        .parent()
        .expect("exe has no parent directory")
        .to_path_buf();

    std::fs::create_dir_all(base_dir.join("logs")).ok();

    let cfg = config::LauncherConfig::load(&base_dir.join("launcher.toml"));
    let specs = process::child_specs(&base_dir, &cfg);

    let quit = Arc::new(AtomicBool::new(false));
    let handles: Vec<process::ChildHandle> = specs
        .into_iter()
        .map(|spec| process::spawn_and_watch(spec, quit.clone()))
        .collect();

    tray::run(&handles, quit.clone());

    // tray::run returned because quit was set (Quit All clicked) — make
    // sure every watcher thread stops trying to restart, then force-kill
    // whichever child is currently running.
    quit.store(true, Ordering::SeqCst);
    for handle in &handles {
        if let Ok(mut guard) = handle.current_child.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.kill();
        }
    }
}
