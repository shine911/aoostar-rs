use crate::process::ChildHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Blocks watching `quit` while it is false, without a tray icon. Used as a
/// fallback if the tray icon itself could not be created (e.g. no desktop
/// session, or the icon resource this build embeds isn't found at runtime).
fn wait_for_quit(quit: &Arc<AtomicBool>) {
    while !quit.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Shows the tray icon (status label + "Quit All" item) and blocks,
/// refreshing the status label every 2 seconds, until `quit` becomes `true`
/// — either because "Quit All" was clicked (this function wires that up
/// itself) or because the caller set it for another reason.
///
/// Never panics: any failure to create or update the tray icon is logged to
/// stderr and this function falls back to just waiting on `quit`, so a
/// tray-creation problem degrades the launcher rather than crashing it.
#[cfg(windows)]
pub fn run(handles: &[ChildHandle], quit: Arc<AtomicBool>) {
    let initial_label = status_label(handles);

    // `IconSource::Resource(name)` is the only usable variant here. On
    // Windows, `tray-item` 0.10.0's `IconSource` has just two variants:
    // `Resource(&str)` (looks up a named icon resource embedded in *this*
    // exe via `LoadImageW`) and `RawIcon(HICON)` (needs an already-built
    // Win32 icon handle). Building a `HICON` from raw pixel bytes requires
    // unsafe FFI (`CreateIconIndirect`/`CreateIcon`), which this crate has
    // no safe wrapper for and which `main.rs`'s crate-level
    // `#![deny(unsafe_code)]` forbids us from calling directly; the
    // `IconSource::Data { .. }` variant that would otherwise take raw bytes
    // is `#[cfg(any(target_os = "macos", ...))]` only and doesn't exist for
    // Windows at all (confirmed by reading the crate's source directly,
    // since docs.rs failed to build docs for this version). So there is no
    // way to construct a tray icon from in-memory bytes on this platform
    // without either unsafe code or a bundled `.ico` resource.
    //
    // No icon resource named "aster-launcher" is currently embedded (see
    // `build.rs`, which only embeds the manifest) so this will realistically
    // return `Err` until a future task adds one via
    // `winresource::WindowsResource::set_icon(...)`. The `Err` arm below
    // degrades gracefully (log + keep running without a tray) rather than
    // panicking, so that gap doesn't take down child-process supervision.
    let mut tray = match TrayItem::new(&initial_label, IconSource::Resource("aster-launcher")) {
        Ok(tray) => tray,
        Err(err) => {
            eprintln!("aster-launcher: failed to create tray icon, running without one: {err}");
            wait_for_quit(&quit);
            return;
        }
    };

    // `TrayItem::add_label` (the crate's public, cross-platform wrapper)
    // doesn't hand back a menu item id, and there is no public "update
    // label" API on `TrayItem` itself. `inner_mut()` exposes the
    // platform-specific implementation, which on Windows does support
    // updating a menu item by id (`add_label_with_id` / `set_label`), so we
    // use that to refresh the status line in place instead of tearing down
    // and rebuilding the whole tray icon on every health change.
    let label_id = match tray.inner_mut().add_label_with_id(&initial_label) {
        Ok(id) => Some(id),
        Err(err) => {
            eprintln!("aster-launcher: failed to add tray status label: {err}");
            None
        }
    };

    {
        let quit_for_menu = quit.clone();
        if let Err(err) = tray.add_menu_item("Quit All", move || {
            quit_for_menu.store(true, Ordering::SeqCst);
        }) {
            eprintln!("aster-launcher: failed to add tray Quit All item: {err}");
        }
    }

    let mut last_label = initial_label;
    while !quit.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(2));

        let label = status_label(handles);
        if label != last_label {
            if let Some(id) = label_id
                && let Err(err) = tray.inner_mut().set_label(&label, id)
            {
                eprintln!("aster-launcher: failed to update tray status label: {err}");
            }
            last_label = label;
        }
    }
}
