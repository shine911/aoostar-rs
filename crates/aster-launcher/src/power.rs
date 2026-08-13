// SPDX-License-Identifier: MIT OR Apache-2.0

//! Windows power-event monitoring (suspend/resume).
//!
//! The launcher suspends all child processes while the machine sleeps and
//! respawns them after wake. Two goals:
//! - no process holds the AOOSTAR LCD serial port across a sleep/resume
//!   cycle (a stale handle is the classic cause of "COM3 dead after wake");
//! - the periodic sensor/refresh loops stop running during sleep (saves
//!   battery on Modern Standby machines).
//!
//! Transport: `RegisterSuspendResumeNotification` — a windowless API that
//! works on Modern Standby (S0 low-power idle). The earlier hidden-window +
//! `WM_POWERBROADCAST` approach never delivered events on the AOOSTAR WTR
//! MAX (launcher.log stayed empty across real sleep/wake cycles), so it was
//! replaced. The system invokes our callback on a private thread; the
//! callback only records the event type and signals a Win32 event, and a
//! dedicated daemon thread does the actual work (kill children / wait /
//! UART restart).

// `main.rs` denies `unsafe_code` crate-wide; this module is the narrow,
// deliberate exception — it is pure Win32 FFI glue. Scoped here so the
// crate-wide deny still guards every other module.
#![allow(unsafe_code)]

// Only used by the `#[cfg(windows)]` code below; gated so the module stays
// warning-free on non-Windows targets (Linux CI).
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Classification of a suspend/resume notification type.
///
/// Platform-free so the logic is unit-testable everywhere; the raw values
/// match the Win32 constants from `windows-sys`:
/// suspend: `PBT_APMSUSPEND` (4), `PBT_APMSTANDBY` (5);
/// resume: `PBT_APMRESUMECRITICAL` (6), `PBT_APMRESUMESUSPEND` (7),
/// `PBT_APMRESUMESTANDBY` (8), `PBT_APMRESUMEAUTOMATIC` (18).
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

#[cfg(windows)]
use crate::process::ChildHandle;

#[cfg(windows)]
struct PowerState {
    suspended: Arc<AtomicBool>,
    handles: Arc<Vec<ChildHandle>>,
    restart_uart_on_resume: bool,
    log_path: PathBuf,
}

/// State shared between the power callback (invoked by the system on a
/// private thread) and the daemon thread. The callback only touches
/// `pending_event` and `wake`; `power` is owned by the daemon thread.
#[cfg(windows)]
#[repr(C)]
struct Shared {
    pending_event: AtomicU32,
    wake: windows_sys::Win32::Foundation::HANDLE,
    power: PowerState,
}

/// Callback invoked by `RegisterSuspendResumeNotification` on a system
/// thread. Must not block: it only records the event type and signals the
/// daemon thread, which does the actual work.
#[cfg(windows)]
unsafe extern "system" fn on_power_event(
    context: *const core::ffi::c_void,
    r#type: u32,
    _setting: *const core::ffi::c_void,
) -> u32 {
    use windows_sys::Win32::System::Threading::SetEvent;

    if let Some(shared) = unsafe { (context as *const Shared).as_ref() } {
        shared.pending_event.store(r#type, Ordering::SeqCst);
        // SAFETY: `wake` is a valid event handle created in `start`; the
        // module carries the scoped `#![allow(unsafe_code)]`.
        unsafe { SetEvent(shared.wake) };
    }
    0 // NO_ERROR
}

/// Starts the power-monitor thread. The thread is a daemon: it runs for the
/// process lifetime and needs no shutdown coordination (it only writes to
/// the launcher log and manipulates child handles, never spawns).
#[cfg(windows)]
pub(crate) fn start(
    suspended: Arc<AtomicBool>,
    handles: Arc<Vec<ChildHandle>>,
    restart_uart_on_resume: bool,
    log_path: PathBuf,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Power::{
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, RegisterSuspendResumeNotification,
        };
        use windows_sys::Win32::System::Threading::{
            CreateEventW, INFINITE, ResetEvent, WaitForSingleObject,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

        unsafe {
            let wake = CreateEventW(
                std::ptr::null(),
                1, /* manual reset */
                0,
                std::ptr::null(),
            );
            if wake.is_null() {
                crate::logging::append_line(
                    &log_path,
                    "power: CreateEventW failed; power handling disabled",
                );
                return;
            }

            let shared = Shared {
                pending_event: AtomicU32::new(0),
                wake,
                power: PowerState {
                    suspended,
                    handles,
                    restart_uart_on_resume,
                    log_path,
                },
            };

            let params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(on_power_event),
                Context: &shared as *const Shared as *mut core::ffi::c_void,
            };
            let registration = RegisterSuspendResumeNotification(
                &params as *const DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as *mut core::ffi::c_void,
                DEVICE_NOTIFY_CALLBACK,
            );
            if registration == 0 {
                crate::logging::append_line(
                    &shared.power.log_path,
                    "power: RegisterSuspendResumeNotification failed; power handling disabled",
                );
                return;
            }

            // Daemon loop: block until the callback signals a power event.
            loop {
                let ret = WaitForSingleObject(shared.wake, INFINITE);
                if ret == WAIT_FAILED {
                    crate::logging::append_line(
                        &shared.power.log_path,
                        "power: WaitForSingleObject failed; power handling disabled",
                    );
                    return;
                }
                if ret == WAIT_OBJECT_0 {
                    ResetEvent(shared.wake);
                }
                let event_type = shared.pending_event.swap(0, Ordering::SeqCst);
                if event_type != 0 {
                    handle_power_event(&shared.power, event_type as usize);
                }
            }
        }
    })
}

#[cfg(windows)]
fn handle_power_event(state: &PowerState, event_type: usize) {
    match classify_power_event(event_type) {
        PowerEvent::Suspend => {
            crate::logging::append_line(
                &state.log_path,
                "power: sleep detected, suspending children",
            );
            state.suspended.store(true, Ordering::SeqCst);
            crate::process::kill_all(&state.handles);
        }
        PowerEvent::Resume => {
            // Only the first Resume after a Suspend may act: Windows can
            // send several Resume notifications for one wake (e.g. 7 then
            // 18), and a second pass would disable the UART while asterctl
            // already has the COM port open again. The swap also records
            // the Suspend→Resume transition.
            if !state.suspended.swap(false, Ordering::SeqCst) {
                crate::logging::append_line(
                    &state.log_path,
                    "power: resume already handled, ignoring",
                );
                return;
            }
            crate::logging::append_line(
                &state.log_path,
                "power: wake detected, waiting for USB stack",
            );
            std::thread::sleep(std::time::Duration::from_secs(RESUME_SETTLE_SECS));
            if state.restart_uart_on_resume {
                crate::logging::append_line(
                    &state.log_path,
                    "power: restarting AOOSTAR USB UART (disable/enable)",
                );
                match crate::device::restart_uart(
                    crate::device::AOOSTAR_UART_VID,
                    crate::device::AOOSTAR_UART_PID,
                ) {
                    Ok(()) => {
                        crate::logging::append_line(&state.log_path, "power: USB UART restarted")
                    }
                    Err(e) => crate::logging::append_line(
                        &state.log_path,
                        &format!("power: USB UART restart failed: {e:?}"),
                    ),
                }
            }
            crate::logging::append_line(&state.log_path, "power: resuming children");
        }
        PowerEvent::Other => {}
    }
}

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
