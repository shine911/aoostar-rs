// SPDX-License-Identifier: MIT OR Apache-2.0

//! Windows power-event monitoring.
//!
//! The launcher suspends all child processes while the machine sleeps and
//! respawns them after wake. Two goals:
//! - no process holds the AOOSTAR LCD serial port across a sleep/resume
//!   cycle (a stale handle is the classic cause of "COM3 dead after wake");
//! - the periodic sensor/refresh loops stop running during sleep (saves
//!   battery on Modern Standby machines).

// `main.rs` denies `unsafe_code` crate-wide; this module is the narrow,
// deliberate exception — it is pure Win32 FFI glue. Scoped here so the
// crate-wide deny still guards every other module.
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Classification of a `WM_POWERBROADCAST` wParam.
///
/// Platform-free so the logic is unit-testable everywhere; the raw values
/// match the Win32 constants `PBT_APMSUSPEND` (4), `PBT_APMRESUMEAUTOMATIC`
/// (18) and `PBT_APMRESUMESUSPEND` (7) from
/// `windows-sys::Win32::UI::WindowsAndMessaging`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerEvent {
    Suspend,
    Resume,
    Other,
}

pub(crate) fn classify_power_event(wparam: usize) -> PowerEvent {
    match wparam {
        // PBT_APMSUSPEND (4), PBT_APMSTANDBY (5)
        4 | 5 => PowerEvent::Suspend,
        // PBT_APMRESUMECRITICAL (6), PBT_APMRESUMESUSPEND (7),
        // PBT_APMRESUMESTANDBY (8), PBT_APMRESUMEAUTOMATIC (18)
        6 | 7 | 8 | 18 => PowerEvent::Resume,
        _ => PowerEvent::Other,
    }
}

/// Seconds to wait after wake before clearing the suspend flag: the USB
/// stack needs a moment to re-enumerate the LCD UART.
const RESUME_SETTLE_SECS: u64 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_suspend_and_resume_wparams() {
        assert_eq!(classify_power_event(4), PowerEvent::Suspend); // PBT_APMSUSPEND
        assert_eq!(classify_power_event(5), PowerEvent::Suspend); // PBT_APMSTANDBY
        assert_eq!(classify_power_event(6), PowerEvent::Resume); // PBT_APMRESUMECRITICAL
        assert_eq!(classify_power_event(7), PowerEvent::Resume); // PBT_APMRESUMESUSPEND
        assert_eq!(classify_power_event(8), PowerEvent::Resume); // PBT_APMRESUMESTANDBY
        assert_eq!(classify_power_event(18), PowerEvent::Resume); // PBT_APMRESUMEAUTOMATIC
    }

    #[test]
    fn classifies_unrelated_messages_as_other() {
        assert_eq!(classify_power_event(0), PowerEvent::Other);
        assert_eq!(classify_power_event(12345), PowerEvent::Other);
    }
}

#[cfg(windows)]
use crate::process::ChildHandle;

// `HWND`, `WPARAM`, `LPARAM` and `LRESULT` are used by `wnd_proc`'s
// signature at module scope, so they are imported here (cfg-gated like the
// rest of the Windows code) rather than inside `start`'s closure.
#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

#[cfg(windows)]
struct PowerState {
    suspended: Arc<AtomicBool>,
    handles: Arc<Vec<ChildHandle>>,
    log_path: PathBuf,
}

// `WndProc` is a plain function pointer, so the state it needs lives in a
// thread-local set once at thread start. The message loop runs on this same
// thread, so there is no cross-thread access.
//
// Edition 2024 requires explicit `unsafe` blocks for unsafe operations even
// inside an `unsafe fn`; this module's scoped `#![allow(unsafe_code)]`
// covers the block.
#[cfg(windows)]
thread_local! {
    static STATE: std::cell::RefCell<Option<PowerState>> = const { std::cell::RefCell::new(None) };
}

/// Starts the power-monitor thread. The thread is a daemon: it runs for the
/// process lifetime and needs no shutdown coordination (it only writes to
/// the launcher log and manipulates child handles, never spawns).
#[cfg(windows)]
pub(crate) fn start(
    suspended: Arc<AtomicBool>,
    handles: Arc<Vec<ChildHandle>>,
    log_path: PathBuf,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // `use` locally so power.rs still compiles on non-Windows targets
        // (windows-sys is a Windows-only dependency).
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DispatchMessageW, GetMessageW, MSG, RegisterClassW, TranslateMessage,
            WNDCLASSW,
        };

        let log_path_for_errors = log_path.clone();
        STATE.with(|slot| {
            *slot.borrow_mut() = Some(PowerState {
                suspended,
                handles,
                log_path,
            });
        });

        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let class_name: Vec<u16> = "AsterLauncherPowerWindow\0".encode_utf16().collect();
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            if RegisterClassW(&wc) == 0 {
                crate::logging::append_line(
                    &log_path_for_errors,
                    "power: RegisterClassW failed; power handling disabled",
                );
                return;
            }
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                crate::logging::append_line(
                    &log_path_for_errors,
                    "power: CreateWindowExW failed; power handling disabled",
                );
                return;
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    })
}

#[cfg(windows)]
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_POWERBROADCAST};
    if msg == WM_POWERBROADCAST {
        STATE.with(|slot| {
            if let Some(state) = slot.borrow().as_ref() {
                handle_power_event(state, wparam);
            }
        });
    }
    // Forward to the default handler. `DefWindowProcW` is unsafe; edition 2024
    // requires an explicit block even inside this `unsafe fn`.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(windows)]
fn handle_power_event(state: &PowerState, wparam: usize) {
    match classify_power_event(wparam) {
        PowerEvent::Suspend => {
            crate::logging::append_line(
                &state.log_path,
                "power: sleep detected, suspending children",
            );
            state.suspended.store(true, Ordering::SeqCst);
            crate::process::kill_all(&state.handles);
        }
        PowerEvent::Resume => {
            crate::logging::append_line(
                &state.log_path,
                "power: wake detected, waiting for USB stack",
            );
            std::thread::sleep(std::time::Duration::from_secs(RESUME_SETTLE_SECS));
            state.suspended.store(false, Ordering::SeqCst);
            crate::logging::append_line(&state.log_path, "power: resuming children");
        }
        PowerEvent::Other => {}
    }
}
