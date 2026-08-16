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
//! The default wake path is a soft protocol-level re-init: children respawn
//! with fresh serial handles and `asterctl` re-sends the OpenTFT (0x0B)
//! handshake, which is exactly how the panel is (re)initialized on the wire
//! (see the reverse-engineered protocol in the `gem10-miniscreen` docs) — no
//! USB reset is involved. Only when `restart_uart_on_resume` is set does the
//! launcher additionally power-cycle the USB UART (the old Device Manager
//! workaround, kept as an opt-in escape hatch).
//!
//! On suspend the LCD is blanked first (CloseTFT 0x0A, via the
//! `cfg/display.state` file) and asterctl gets a short grace to apply it
//! before the children are killed — the same blank-then-kill sequence the
//! clean-quit path uses — so the panel enters sleep deterministically off
//! and wake re-initializes it cleanly with OpenTFT.
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

/// Seconds to wait after writing "off" on suspend so asterctl can send
/// CloseTFT (0x0A) before it is killed. Covers asterctl's ~1s state-file
/// poll plus a mid-render margin (a full frame upload can take ~1.5s);
/// same value as the clean-quit grace (`QUIT_KILL_GRACE_SECS`).
#[cfg(windows)]
const SUSPEND_BLANK_GRACE_SECS: u64 = 2;

#[cfg(windows)]
use crate::process::ChildHandle;

#[cfg(windows)]
struct PowerState {
    suspended: Arc<AtomicBool>,
    handles: Arc<Vec<ChildHandle>>,
    /// LCD display control: `suspend()`/`resume()` blank the display
    /// (CloseTFT) before the children are killed and re-arm it on wake.
    display: Arc<crate::display::DisplayControl>,
    restart_uart_on_resume: bool,
    log_path: PathBuf,
    /// True once a Resume has been acted on for the current sleep cycle.
    /// Daemon-thread-only (a plain bool): Windows can send several Resume
    /// notifications for one wake (e.g. 7 then 18) and only the first may
    /// act. Reset by the next Suspend; starts true so a Resume with no
    /// preceding Suspend is ignored (same as the previous `swap(false)`
    /// guard on `suspended`).
    resume_handled: bool,
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
    display: Arc<crate::display::DisplayControl>,
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

            let mut shared = Shared {
                pending_event: AtomicU32::new(0),
                wake,
                power: PowerState {
                    suspended,
                    handles,
                    display,
                    restart_uart_on_resume,
                    log_path,
                    resume_handled: true,
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
                    handle_power_event(&mut shared.power, event_type as usize);
                }
            }
        }
    })
}

#[cfg(windows)]
fn handle_power_event(state: &mut PowerState, event_type: usize) {
    match classify_power_event(event_type) {
        PowerEvent::Suspend => {
            crate::logging::append_line(
                &state.log_path,
                "power: sleep detected, suspending children",
            );
            // Arm the Resume handler for the next wake.
            state.resume_handled = false;
            // Blank the LCD before killing asterctl: write "off" (asterctl
            // sends CloseTFT 0x0A) and give it the grace to apply it —
            // mirroring the clean-quit sequence. `suspended` must stay
            // false during the grace, or the watchers would force-kill
            // asterctl before it can blank the display.
            state.display.suspend();
            std::thread::sleep(std::time::Duration::from_secs(SUSPEND_BLANK_GRACE_SECS));
            state.suspended.store(true, Ordering::SeqCst);
            crate::process::kill_all(&state.handles, &state.log_path);
        }
        PowerEvent::Resume => {
            // Only the first Resume after a Suspend may act: Windows can
            // send several Resume notifications for one wake (e.g. 7 then
            // 18), and a second pass would re-restart the UART while a
            // child already has the COM port open again.
            if state.resume_handled {
                crate::logging::append_line(
                    &state.log_path,
                    "power: resume already handled, ignoring",
                );
                return;
            }
            state.resume_handled = true;
            crate::logging::append_line(
                &state.log_path,
                "power: wake detected, waiting for USB stack",
            );
            std::thread::sleep(std::time::Duration::from_secs(RESUME_SETTLE_SECS));
            if state.restart_uart_on_resume {
                // Opt-in escape hatch: the default wake path relies on the
                // children's fresh serial handles + OpenTFT re-init, which
                // the panel protocol supports directly. Some units need the
                // hard USB re-enumeration instead.
                crate::logging::append_line(&state.log_path, "power: resetting AOOSTAR USB UART");
                match crate::device::restart_uart(
                    crate::device::AOOSTAR_UART_VID,
                    crate::device::AOOSTAR_UART_PID,
                ) {
                    Ok(crate::device::RestartMethod::Reset) => crate::logging::append_line(
                        &state.log_path,
                        "power: USB UART restarted (CM_Reset_Device)",
                    ),
                    Ok(crate::device::RestartMethod::DisableEnable) => crate::logging::append_line(
                        &state.log_path,
                        "power: USB UART restarted (disable/enable fallback)",
                    ),
                    Err(e) => crate::logging::append_line(
                        &state.log_path,
                        &format!("power: USB UART restart failed: {e:?}"),
                    ),
                }
            }
            // Re-arm the display control for the new wake: clear the
            // suspend override and rewrite the current mode's state, so a
            // respawned asterctl sees "on" on its first poll (no waiting
            // for the next ~2s heartbeat).
            state.display.resume();
            // With `restart_uart_on_resume` the children must NOT respawn
            // before the UART restart: asterctl reopening COM3 while the
            // device is being disabled is what makes CM_Disable_DevNode fail
            // (CR_INSUFFICIENT_RESOURCES), leaving the port wedged for the
            // rest of the session. Keep `suspended` set (watchers keep
            // sleeping) until the UART has been re-enumerated, then clear it
            // so the children start with fresh serial handles. By default
            // (no UART reset) the settle wait above is all that stands
            // between wake and the children's soft OpenTFT re-init.
            crate::logging::append_line(&state.log_path, "power: resuming children");
            state.suspended.store(false, Ordering::SeqCst);
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
