// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::process::{ChildHandle, ChildSpec};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tray_item::{IconSource, TrayItem};

/// Builds the tray status line summarizing child health: "all running" or a
/// "degraded (...)" list of the children currently reporting unhealthy.
fn status_label(handles: &[ChildHandle]) -> String {
    let down: Vec<&str> = handles
        .iter()
        .filter(|h| !h.healthy.load(Ordering::SeqCst))
        .map(|h| h.name)
        .collect();

    if down.is_empty() {
        "Aster Launcher: all running".to_string()
    } else {
        format!("Aster Launcher: degraded ({})", down.join(", "))
    }
}

/// Tray "Refresh: Ns" click handler: persists `secs` to `launcher.toml`,
/// rebuilds the child specs with the new interval, and restarts
/// aster-sysinfo + hwbridge so it applies immediately (their watchers read
/// the specs fresh on every spawn). `current` is updated so the tray menu
/// can move the check mark.
#[cfg(windows)]
fn apply_refresh(
    secs: u16,
    config_path: &Path,
    base_dir: &Path,
    specs: &Mutex<[ChildSpec; 3]>,
    handles: &[ChildHandle],
    log_path: &Path,
    current: &AtomicU16,
) {
    // 1. persist the choice so it survives a launcher restart
    if let Err(err) = crate::config::set_refresh_time(config_path, secs) {
        crate::logging::append_line(
            log_path,
            &format!(
                "failed to write refresh_time={secs}s to {}: {err}",
                config_path.display()
            ),
        );
        return;
    }
    crate::logging::append_line(
        log_path,
        &format!("tray: refresh_time set to {secs}s; restarting aster-sysinfo and hwbridge"),
    );

    // 2. rebuild the specs with the new shared interval
    // Reload from disk so the OTHER tray option chosen since startup
    // (theme <-> refresh) survives in the rebuilt specs.
    let mut new_cfg = crate::config::LauncherConfig::load(config_path);
    new_cfg.refresh_time = Some(secs);
    let mut guard = match specs.lock() {
        Ok(guard) => guard,
        Err(err) => {
            crate::logging::append_line(
                log_path,
                &format!("child specs mutex poisoned, refusing to restart children: {err}"),
            );
            return;
        }
    };
    *guard = crate::process::child_specs(base_dir, &new_cfg);

    // 3. kill the two refresh-driven children; their watchers respawn them
    //    with the updated arguments within ~2s
    crate::process::kill_named(handles, &["aster-sysinfo", "hwbridge"]);

    current.store(secs, Ordering::SeqCst);
}

/// Tray "Themes" click handler: persists `theme` to `launcher.toml`,
/// rebuilds the child specs with the new theme, and restarts asterctl so it
/// applies immediately (its watcher reads the specs fresh on every spawn).
/// `current` is updated so the tray menu can move the check mark.
#[cfg(windows)]
fn apply_theme(
    theme: u16,
    config_path: &Path,
    base_dir: &Path,
    specs: &Mutex<[ChildSpec; 3]>,
    handles: &[ChildHandle],
    log_path: &Path,
    current: &AtomicU16,
) {
    // 1. persist the choice so it survives a launcher restart
    if let Err(err) = crate::config::set_theme(config_path, theme) {
        crate::logging::append_line(
            log_path,
            &format!(
                "failed to write theme={theme} to {}: {err}",
                config_path.display()
            ),
        );
        return;
    }
    crate::logging::append_line(
        log_path,
        &format!("tray: theme set to {theme}; restarting asterctl"),
    );

    // 2. rebuild the specs with the new theme
    // Reload from disk so the OTHER tray option chosen since startup
    // (theme <-> refresh) survives in the rebuilt specs.
    let mut new_cfg = crate::config::LauncherConfig::load(config_path);
    new_cfg.theme = Some(theme);
    let mut guard = match specs.lock() {
        Ok(guard) => guard,
        Err(err) => {
            crate::logging::append_line(
                log_path,
                &format!("child specs mutex poisoned, refusing to restart children: {err}"),
            );
            return;
        }
    };
    *guard = crate::process::child_specs(base_dir, &new_cfg);

    // 3. kill asterctl; its watcher respawns it with the updated args
    crate::process::kill_named(handles, &["asterctl"]);

    current.store(theme, Ordering::SeqCst);
}

/// Shows the tray icon (status label, "Refresh time" sub-menu, "Themes"
/// sub-menu, "Display" sub-menu, "Quit" item) and blocks, refreshing the
/// status label and the hover tooltip every 2 seconds, until `quit`
/// becomes `true` — either because "Quit" was clicked (this function wires
/// that up itself) or because the caller set it for another reason.
///
/// The "Refresh time" sub-menu entries write the chosen interval to
/// `config_path` and restart `aster-sysinfo` + `hwbridge` via
/// `specs`/`handles` (see [`apply_refresh`]); `current_refresh` tracks the
/// active interval so the sub-menu shows a check mark on it.
///
/// The "Display" sub-menu selects the LCD display mode (On / Off / Follow
/// screen state) through `display` (see
/// [`crate::display::DisplayControl`]): the choice is persisted to
/// `config_path` and applied immediately via `cfg/display.state`, which
/// the panel-mode asterctl polls.
///
/// Never panics: any failure to create or update the tray icon is logged to
/// `log_path` (this exe is built with `windows_subsystem = "windows"`, so it
/// has no console for `eprintln!` to reach). If the tray icon itself cannot
/// be created there is no user-visible control surface at all, so this
/// function returns immediately rather than blocking — the caller's shutdown
/// path then kills the children instead of leaving hidden elevated processes
/// running with no way to stop them.
#[cfg(windows)]
pub fn run(
    handles: &[ChildHandle],
    specs: Arc<Mutex<[ChildSpec; 3]>>,
    current_refresh: Arc<AtomicU16>,
    current_theme: Arc<AtomicU16>,
    display: Arc<crate::display::DisplayControl>,
    quit: Arc<AtomicBool>,
    log_path: &Path,
    config_path: &Path,
    base_dir: &Path,
) {
    let initial_label = status_label(handles);

    // `build.rs` embeds `aster-launcher.ico` into this exe as a named icon
    // resource via `res.set_icon_with_id("aster-launcher.ico",
    // "aster-launcher")`, matching the resource name `LoadImageW` looks up
    // here (Windows resource name lookup is case-insensitive, so the
    // compiler-normalized `'ASTER-LAUNCHER'` resource name still matches
    // this lowercase string). The `Err` arm below is still a real, live
    // fallback path (not just a leftover from before the icon existed): a
    // corrupted resource section or an icon load failure on some odd
    // environment can still happen, and must not leave the launcher running
    // invisibly.
    let mut tray = match TrayItem::new(&initial_label, IconSource::Resource("aster-launcher")) {
        Ok(tray) => tray,
        Err(err) => {
            crate::logging::append_line(
                log_path,
                &format!(
                    "failed to create tray icon: {err} — shutting down, \
                     because without a tray there is no way to stop the child processes"
                ),
            );
            return;
        }
    };

    // `TrayItem::add_label` (the crate's public, cross-platform wrapper)
    // doesn't hand back a menu item id, and there is no public "update
    // label" API on `TrayItem` itself. `inner_mut()` exposes the
    // platform-specific implementation, which on Windows does support
    // updating a menu item by id (`add_label_with_id` / `set_label`) and
    // updating the hover tooltip (`set_tooltip`), so we use that to refresh
    // the status in place instead of tearing down and rebuilding the whole
    // tray icon on every health change.
    let label_id = match tray.inner_mut().add_label_with_id(&initial_label) {
        Ok(id) => Some(id),
        Err(err) => {
            crate::logging::append_line(
                log_path,
                &format!("failed to add tray status label: {err}"),
            );
            None
        }
    };

    // "Refresh time" sub-menu: picking an interval persists it to
    // launcher.toml and restarts aster-sysinfo + hwbridge so it applies
    // immediately. The active interval carries a native check mark.
    let active_refresh = current_refresh.load(Ordering::SeqCst);
    let mut refresh_submenu: Option<u32> = None;
    let mut refresh_ids = [0u32; crate::config::REFRESH_OPTIONS.len()];
    match tray.inner_mut().add_submenu("Refresh time") {
        Ok(sub) => {
            refresh_submenu = Some(sub);
            for (i, secs) in crate::config::REFRESH_OPTIONS.iter().enumerate() {
                let secs = *secs;
                let config_path = config_path.to_path_buf();
                let base_dir = base_dir.to_path_buf();
                let specs = specs.clone();
                let handles = handles.to_vec();
                let log_path = log_path.to_path_buf();
                // separate clone for the error arm below (the original is
                // moved into the menu closure)
                let log_path_err = log_path.clone();
                let current = current_refresh.clone();
                let label = format!("{secs}s");
                match tray
                    .inner_mut()
                    .add_submenu_item_with_id(sub, &label, move || {
                        apply_refresh(
                            secs,
                            &config_path,
                            &base_dir,
                            &specs,
                            &handles,
                            &log_path,
                            &current,
                        );
                    }) {
                    Ok(id) => refresh_ids[i] = id,
                    Err(err) => crate::logging::append_line(
                        &log_path_err,
                        &format!("failed to add tray refresh menu item ({secs}s): {err}"),
                    ),
                }
            }
            // Check the currently active interval.
            if let Some(pos) = crate::config::REFRESH_OPTIONS
                .iter()
                .position(|&v| v == active_refresh)
            {
                let _ = tray
                    .inner_mut()
                    .set_submenu_item_checked(sub, refresh_ids[pos], true);
            }
        }
        Err(err) => crate::logging::append_line(
            log_path,
            &format!("failed to add tray Refresh time submenu: {err}"),
        ),
    }

    // "Themes" sub-menu: picking a theme persists it to launcher.toml and
    // restarts asterctl so it applies immediately (the watcher respawns it
    // with the new `--theme` argument). The active theme carries a check mark.
    let active_theme = current_theme.load(Ordering::SeqCst);
    let mut theme_submenu: Option<u32> = None;
    let mut theme_ids = [0u32; crate::config::THEME_OPTIONS.len()];
    match tray.inner_mut().add_submenu("Themes") {
        Ok(sub) => {
            theme_submenu = Some(sub);
            for (i, (idx, label)) in crate::config::THEME_OPTIONS.iter().enumerate() {
                let idx = *idx;
                let config_path = config_path.to_path_buf();
                let base_dir = base_dir.to_path_buf();
                let specs = specs.clone();
                let handles = handles.to_vec();
                let log_path = log_path.to_path_buf();
                // separate clone for the error arm below (the original is
                // moved into the menu closure)
                let log_path_err = log_path.clone();
                let current = current_theme.clone();
                let label = label.to_string();
                match tray
                    .inner_mut()
                    .add_submenu_item_with_id(sub, &label, move || {
                        apply_theme(
                            idx,
                            &config_path,
                            &base_dir,
                            &specs,
                            &handles,
                            &log_path,
                            &current,
                        );
                    }) {
                    Ok(id) => theme_ids[i] = id,
                    Err(err) => crate::logging::append_line(
                        &log_path_err,
                        &format!("failed to add tray theme menu item ({label}): {err}"),
                    ),
                }
            }
            // Check the currently active theme.
            if let Some(pos) = crate::config::THEME_OPTIONS
                .iter()
                .position(|&(idx, _)| idx == active_theme)
            {
                let _ = tray
                    .inner_mut()
                    .set_submenu_item_checked(sub, theme_ids[pos], true);
            }
        }
        Err(err) => crate::logging::append_line(
            log_path,
            &format!("failed to add tray Themes submenu: {err}"),
        ),
    }

    // "Display" sub-menu: On / Off / Follow screen state. Picking one
    // persists `display_mode` to launcher.toml and applies it immediately
    // (the mode also writes `cfg/display.state`, which the panel-mode
    // asterctl polls every refresh — no restart needed). The active mode
    // carries a native check mark.
    let active_display = display.mode.load(Ordering::SeqCst);
    let mut display_submenu: Option<u32> = None;
    let mut display_ids = [0u32; crate::config::DISPLAY_OPTIONS.len()];
    match tray.inner_mut().add_submenu("Display") {
        Ok(sub) => {
            display_submenu = Some(sub);
            for (i, (mode, label)) in crate::config::DISPLAY_OPTIONS.iter().enumerate() {
                let mode = *mode;
                let config_path = config_path.to_path_buf();
                let log_path = log_path.to_path_buf();
                // separate clone for the error arm below (the original is
                // moved into the menu closure)
                let log_path_err = log_path.clone();
                let display = display.clone();
                let label = label.to_string();
                match tray
                    .inner_mut()
                    .add_submenu_item_with_id(sub, &label, move || {
                        display.apply(mode, &config_path);
                    }) {
                    Ok(id) => display_ids[i] = id,
                    Err(err) => crate::logging::append_line(
                        &log_path_err,
                        &format!("failed to add tray display menu item ({label}): {err}"),
                    ),
                }
            }
            // Check the currently active mode.
            if let Some(pos) = crate::config::DISPLAY_OPTIONS
                .iter()
                .position(|(mode, _)| mode.index() == active_display)
            {
                let _ = tray
                    .inner_mut()
                    .set_submenu_item_checked(sub, display_ids[pos], true);
            }
        }
        Err(err) => crate::logging::append_line(
            log_path,
            &format!("failed to add tray Display submenu: {err}"),
        ),
    }

    // "Quit" item at the bottom of the menu: stops all children and exits
    // the launcher (each child watcher observes `quit` and shuts down).
    {
        let quit_for_menu = quit.clone();
        if let Err(err) = tray.add_menu_item("Quit", move || {
            quit_for_menu.store(true, Ordering::SeqCst);
        }) {
            crate::logging::append_line(log_path, &format!("failed to add tray Quit item: {err}"));
        }
    }

    let mut last_label = initial_label;
    let mut last_refresh = active_refresh;
    let mut last_theme = active_theme;
    let mut last_display = active_display;
    while !quit.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(2));

        // Heartbeat for asterctl's launcher-death watchdog: rewrite
        // cfg/display.state (idempotent content) so its mtime stays fresh
        // while the launcher runs. If this loop stops — launcher closed or
        // killed — asterctl sees the stale file, switches the display off
        // and exits.
        display.heartbeat();

        // Move the check mark when the interval changed (e.g. the user
        // picked a different one from the "Refresh time" submenu).
        let refresh = current_refresh.load(Ordering::SeqCst);
        if refresh != last_refresh {
            if let Some(sub) = refresh_submenu {
                for (i, id) in refresh_ids.iter().enumerate() {
                    if *id != 0
                        && let Err(err) = tray.inner_mut().set_submenu_item_checked(
                            sub,
                            *id,
                            crate::config::REFRESH_OPTIONS[i] == refresh,
                        )
                    {
                        crate::logging::append_line(
                            log_path,
                            &format!("failed to update refresh menu check mark: {err}"),
                        );
                        break;
                    }
                }
            }
            last_refresh = refresh;
        }

        // Move the check mark when the theme changed (e.g. the user picked a
        // different one from the "Themes" submenu).
        let theme = current_theme.load(Ordering::SeqCst);
        if theme != last_theme {
            if let Some(sub) = theme_submenu {
                for (i, id) in theme_ids.iter().enumerate() {
                    if *id != 0
                        && let Err(err) = tray.inner_mut().set_submenu_item_checked(
                            sub,
                            *id,
                            crate::config::THEME_OPTIONS[i].0 == theme,
                        )
                    {
                        crate::logging::append_line(
                            log_path,
                            &format!("failed to update theme menu check mark: {err}"),
                        );
                        break;
                    }
                }
            }
            last_theme = theme;
        }

        // Move the check mark when the display mode changed (e.g. the user
        // picked a different one from the "Display" submenu).
        let mode = display.mode.load(Ordering::SeqCst);
        if mode != last_display {
            if let Some(sub) = display_submenu {
                for (i, id) in display_ids.iter().enumerate() {
                    if *id != 0
                        && let Err(err) = tray.inner_mut().set_submenu_item_checked(
                            sub,
                            *id,
                            crate::config::DISPLAY_OPTIONS[i].0.index() == mode,
                        )
                    {
                        crate::logging::append_line(
                            log_path,
                            &format!("failed to update display menu check mark: {err}"),
                        );
                        break;
                    }
                }
            }
            last_display = mode;
        }

        let label = status_label(handles);
        if label != last_label {
            if let Some(id) = label_id
                && let Err(err) = tray.inner_mut().set_label(&label, id)
            {
                crate::logging::append_line(
                    log_path,
                    &format!("failed to update tray status label: {err}"),
                );
            }
            // The tooltip is what `TrayItem::new` set from the startup
            // label, when all 3 children were still coming up — refresh it
            // too, or hovering the icon would report "degraded" forever.
            if let Err(err) = tray.inner_mut().set_tooltip(&label) {
                crate::logging::append_line(
                    log_path,
                    &format!("failed to update tray tooltip: {err}"),
                );
            }
            last_label = label;
        }
    }
}
