// SPDX-License-Identifier: MIT OR Apache-2.0

//! LCD display-mode control for the tray "Display" sub-menu: manual on/off
//! and "follow screen state".
//!
//! The launcher drives the LCD through a tiny state file
//! (`cfg/display.state`, containing `on` or `off`) that `asterctl
//! --display-state` polls on every refresh. Manual On/Off just write that
//! file; because the panel-mode asterctl keeps the serial port open, no
//! process restart is needed and the display wakes from the file alone.
//!
//! "Follow screen state" (the third menu mode) mirrors the Windows console
//! display power state into the same file: `PowerSettingRegisterNotification`
//! with `GUID_CONSOLE_DISPLAY_STATE` fires a `PBT_POWERSETTINGCHANGE`
//! notification whenever the display turns off/on/dims (data byte 0 = off,
//! 1 = on, 2 = dimmed). This works on both S3 and Modern Standby (S0ix) and
//! also covers idle-blank / screensaver / lid transitions that never enter
//! system sleep — a plain display-power poll would miss those.
//!
//! The state file doubles as the launcher's heartbeat: the tray loop
//! rewrites it roughly every 2s ([`DisplayControl::heartbeat`]), and
//! asterctl switches the display off — and exits — when the file goes
//! stale, so a closed or killed launcher never leaves the LCD stuck on.
//! On a clean launcher shutdown [`DisplayControl::force_off`] blanks the
//! display before asterctl is killed; while the machine sleeps
//! [`DisplayControl::suspend`] does the same (the heartbeat keeps writing
//! "off" for the whole sleep so asterctl sends CloseTFT before it is
//! killed and a mid-sleep heartbeat cannot flip the file back to "on").

// `main.rs` denies `unsafe_code` crate-wide; this module is the narrow,
// deliberate exception — pure Win32 FFI glue, scoped the same way as
// `power.rs`.
#![allow(unsafe_code)]

use crate::config::DisplayMode;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};

/// `GUID_CONSOLE_DISPLAY_STATE` — the power-setting GUID whose
/// `PBT_POWERSETTINGCHANGE` notifications carry the console display power
/// state (0 = off, 1 = on, 2 = dimmed). Registered once at startup; the
/// watcher only acts while the "Follow screen state" mode is active.
#[cfg(windows)]
const GUID_CONSOLE_DISPLAY_STATE: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

/// `PBT_POWERSETTINGCHANGE` (0x8013): the notification type delivered to
/// power-setting callbacks; its `setting` argument points at a
/// `POWERBROADCAST_SETTING`.
#[cfg(windows)]
const PBT_POWERSETTINGCHANGE: u32 = 0x8013;

/// True when `guid` is [`GUID_CONSOLE_DISPLAY_STATE`]. Field-wise because
/// `windows-sys`'s `GUID` implements no `PartialEq` at this version.
#[cfg(windows)]
fn is_display_state_guid(guid: &windows_sys::core::GUID) -> bool {
    guid.data1 == GUID_CONSOLE_DISPLAY_STATE.data1
        && guid.data2 == GUID_CONSOLE_DISPLAY_STATE.data2
        && guid.data3 == GUID_CONSOLE_DISPLAY_STATE.data3
        && guid.data4 == GUID_CONSOLE_DISPLAY_STATE.data4
}

// `PowerSettingRegisterNotification` lives in `powrprof.dll` (not exposed
// by the `windows-sys` crate at this version), so declare it here — same
// pattern as the rest of this module's narrow FFI surface.
#[cfg(windows)]
#[link(name = "powrprof")]
unsafe extern "system" {
    fn PowerSettingRegisterNotification(
        setting_guid: *const windows_sys::core::GUID,
        flags: u32,
        recipient: *const core::ffi::c_void,
        registration_handle: *mut *mut core::ffi::c_void,
    ) -> u32;

    fn PowerSettingUnregisterNotification(registration_handle: *mut core::ffi::c_void) -> u32;
}

/// True if a Windows console display power state means "screen visible".
/// 0 = off; 1 = on and 2 = dimmed both still show content, so they count
/// as on (the LCD follows only the off transition).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn display_state_is_on(state: u8) -> bool {
    state != 0
}

/// Writes `cfg/display.state` (`on\n` / `off\n`), creating the parent
/// directory if needed. `asterctl --display-state` polls this file and
/// switches the LCD accordingly.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn write_state_file(path: &Path, on: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, if on { "on\n" } else { "off\n" })
}

/// Effective on/off state for a display mode, given the last Windows
/// display power state seen (0 = off, 1 = on, 2 = dimmed). Shared by the
/// startup/menu application and the periodic heartbeat rewrite.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn effective_state_on(mode: DisplayMode, last_seen: u8) -> bool {
    match mode {
        DisplayMode::On => true,
        DisplayMode::Off => false,
        DisplayMode::Follow => display_state_is_on(last_seen),
    }
}

/// Effective state the heartbeat writes: while the machine is asleep
/// (`suspend_off`) it is always off — the LCD must stay blank during
/// sleep, and a heartbeat firing mid-grace must not flip the file back to
/// "on" before asterctl has sent CloseTFT. Otherwise the mode's effective
/// state. Extracted so the suspend override is unit-testable on every
/// platform.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn heartbeat_state_on(mode: DisplayMode, last_seen: u8, suspend_off: bool) -> bool {
    if suspend_off {
        false
    } else {
        effective_state_on(mode, last_seen)
    }
}

/// State shared between the power-setting callback (invoked by the system
/// on a private thread) and the watcher daemon thread. The callback only
/// touches `last_seen` and `wake`; `follow_active` and the state file live
/// on the daemon thread.
#[cfg(windows)]
#[repr(C)]
struct Shared {
    /// Latest console display power state: 0 = off, 1 = on, 2 = dimmed.
    last_seen: AtomicU8,
    wake: windows_sys::Win32::Foundation::HANDLE,
}

/// Callback invoked by `PowerSettingRegisterNotification` on a system
/// thread. Must not block: it only records the display power state and
/// signals the watcher daemon, which does the actual work.
#[cfg(windows)]
unsafe extern "system" fn on_display_power_event(
    context: *const core::ffi::c_void,
    r#type: u32,
    setting: *const core::ffi::c_void,
) -> u32 {
    use windows_sys::Win32::System::Threading::SetEvent;

    if r#type == PBT_POWERSETTINGCHANGE
        && let Some(shared) = unsafe { (context as *const Shared).as_ref() }
        && let Some(pbs) = unsafe {
            (setting as *const windows_sys::Win32::System::Power::POWERBROADCAST_SETTING).as_ref()
        }
        && is_display_state_guid(&pbs.PowerSetting)
    {
        // The payload is a DWORD (0/1/2); the first byte carries the value.
        shared.last_seen.store(pbs.Data[0], Ordering::SeqCst);
        // SAFETY: `wake` is a valid event handle created in
        // `start_follow_watcher`; the module carries the scoped
        // `#![allow(unsafe_code)]`.
        unsafe { SetEvent(shared.wake) };
    }
    0 // NO_ERROR
}

/// Shared controller for the tray "Display" sub-menu and the follow
/// watcher.
///
/// - `mode` carries the active menu index (see [`DisplayMode::index`]) so
///   the tray can move the check mark;
/// - `follow_active` decides whether the watcher may write the state file
///   (only while "Follow screen state" is the selected mode);
/// - `last_seen` tracks the latest Windows display power state so that
///   switching to "Follow" can apply it immediately instead of waiting for
///   the next display transition.
#[cfg(windows)]
pub(crate) struct DisplayControl {
    pub mode: Arc<AtomicU16>,
    follow_active: Arc<AtomicBool>,
    last_seen: Arc<AtomicU8>,
    /// Set while the machine is asleep: forces the heartbeat to keep
    /// writing "off" (see [`heartbeat_state_on`]) so asterctl blanks the
    /// LCD before the launcher kills it and a mid-sleep heartbeat cannot
    /// flip the file back to "on".
    suspend_off: Arc<AtomicBool>,
    state_file: PathBuf,
    log_path: PathBuf,
}

#[cfg(windows)]
impl DisplayControl {
    /// Creates the controller, writes `cfg/display.state` to match
    /// `initial_mode`, and starts the follow watcher daemon. The daemon is
    /// registered once and runs for the process lifetime (like
    /// `power.rs`); it does nothing unless "Follow" is selected, so
    /// starting it always is harmless.
    pub(crate) fn new(
        initial_mode: DisplayMode,
        state_file: PathBuf,
        log_path: PathBuf,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            mode: Arc::new(AtomicU16::new(initial_mode.index())),
            // Default to "on" until the first display-power notification:
            // worst case the LCD stays on briefly instead of flickering off.
            follow_active: Arc::new(AtomicBool::new(false)),
            last_seen: Arc::new(AtomicU8::new(1)),
            suspend_off: Arc::new(AtomicBool::new(false)),
            state_file,
            log_path,
        });
        this.write_for_mode(initial_mode);
        this.start_follow_watcher();
        this
    }

    /// Tray "Display" sub-menu handler: persists `mode` to `config_path`
    /// and applies it immediately. Never panics — a persist failure is
    /// logged (the menu simply does not stick across restarts).
    pub(crate) fn apply(&self, mode: DisplayMode, config_path: &Path) {
        if let Err(err) = crate::config::set_display_mode(config_path, mode) {
            crate::logging::append_line(
                &self.log_path,
                &format!(
                    "failed to write display_mode={mode:?} to {}: {err}",
                    config_path.display()
                ),
            );
            return;
        }
        crate::logging::append_line(
            &self.log_path,
            &format!("tray: display mode set to {mode:?}"),
        );
        self.write_for_mode(mode);
    }

    /// Applies `mode` without touching `launcher.toml` (used at startup and
    /// by `apply` after persisting): updates the menu index, the follow
    /// flag, and the state file asterctl polls.
    fn write_for_mode(&self, mode: DisplayMode) {
        self.mode.store(mode.index(), Ordering::SeqCst);
        match mode {
            DisplayMode::On | DisplayMode::Off => {
                self.follow_active.store(false, Ordering::SeqCst);
            }
            DisplayMode::Follow => {
                self.follow_active.store(true, Ordering::SeqCst);
            }
        }
        let on = effective_state_on(mode, self.last_seen.load(Ordering::SeqCst));
        self.write_state(on);
    }

    /// Periodic heartbeat, called from the tray loop roughly every 2s:
    /// rewrites `cfg/display.state` with the current state (off while the
    /// machine is asleep — see [`heartbeat_state_on`]). The rewrite keeps
    /// the file's mtime fresh, which is what asterctl uses to detect a
    /// dead launcher — when the file goes stale it switches the display
    /// off and exits. The content is idempotent, so the extra writes are
    /// harmless.
    pub(crate) fn heartbeat(&self) {
        let mode = DisplayMode::from_index(self.mode.load(Ordering::SeqCst));
        let on = heartbeat_state_on(
            mode,
            self.last_seen.load(Ordering::SeqCst),
            self.suspend_off.load(Ordering::SeqCst),
        );
        self.write_state(on);
    }

    /// Launcher shutdown: writes `cfg/display.state` as "off" regardless
    /// of the current mode, so asterctl switches the display off before
    /// the launcher kills it (see the shutdown grace in `main.rs`).
    pub(crate) fn force_off(&self) {
        self.write_state(false);
    }

    /// Sleep: blanks the LCD (writes "off") and keeps the heartbeat
    /// writing "off" while the machine is asleep, so asterctl sends
    /// CloseTFT before the launcher kills it and a mid-sleep heartbeat
    /// cannot flip the file back to "on". Called by the power monitor
    /// before its suspend grace (see `power.rs`).
    pub(crate) fn suspend(&self) {
        self.suspend_off.store(true, Ordering::SeqCst);
        self.write_state(false);
    }

    /// Wake: clears the suspend override and rewrites the current mode's
    /// state immediately, so a respawned asterctl sees "on" on its very
    /// first poll instead of waiting for the next ~2s heartbeat.
    pub(crate) fn resume(&self) {
        self.suspend_off.store(false, Ordering::SeqCst);
        let mode = DisplayMode::from_index(self.mode.load(Ordering::SeqCst));
        self.write_state(effective_state_on(
            mode,
            self.last_seen.load(Ordering::SeqCst),
        ));
    }

    fn write_state(&self, on: bool) {
        if let Err(err) = write_state_file(&self.state_file, on) {
            crate::logging::append_line(
                &self.log_path,
                &format!(
                    "failed to write display.state ({}): {err}",
                    self.state_file.display()
                ),
            );
        }
    }

    /// Spawns the follow watcher daemon: registers for
    /// `GUID_CONSOLE_DISPLAY_STATE` power-setting notifications and, while
    /// "Follow screen state" is active, mirrors every display power-state
    /// change into `cfg/display.state`. Never joined (daemon thread, like
    /// `power.rs`).
    fn start_follow_watcher(self: &Arc<Self>) {
        use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            CreateEventW, INFINITE, ResetEvent, WaitForSingleObject,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

        let follow_active = self.follow_active.clone();
        let last_seen = self.last_seen.clone();
        let state_file = self.state_file.clone();
        let log_path = self.log_path.clone();

        std::thread::spawn(move || {
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
                        "display: CreateEventW failed; follow screen state disabled",
                    );
                    return;
                }

                let shared = Shared {
                    last_seen: AtomicU8::new(1),
                    wake,
                };
                let params =
                    windows_sys::Win32::System::Power::DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                        Callback: Some(on_display_power_event),
                        Context: &shared as *const Shared as *mut core::ffi::c_void,
                    };
                let mut registration: *mut core::ffi::c_void = std::ptr::null_mut();
                let status = PowerSettingRegisterNotification(
                    &GUID_CONSOLE_DISPLAY_STATE,
                    DEVICE_NOTIFY_CALLBACK,
                    &params as *const windows_sys::Win32::System::Power::DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS
                        as *const core::ffi::c_void,
                    &mut registration,
                );
                if status != 0 {
                    crate::logging::append_line(
                        &log_path,
                        &format!(
                            "display: PowerSettingRegisterNotification failed ({status}); \
                             follow screen state disabled"
                        ),
                    );
                    return;
                }

                // Daemon loop: block until the callback signals a
                // display-power change.
                loop {
                    let ret = WaitForSingleObject(wake, INFINITE);
                    if ret == WAIT_FAILED {
                        crate::logging::append_line(
                            &log_path,
                            "display: WaitForSingleObject failed; follow screen state disabled",
                        );
                        break;
                    }
                    if ret == WAIT_OBJECT_0 {
                        ResetEvent(wake);
                    }
                    // Publish the state the tray reads when "Follow" is
                    // selected, then mirror it into the state file — but
                    // only while "Follow" is the active mode. Manual On/Off
                    // keep working because they clear `follow_active`
                    // before writing the file themselves.
                    let state = shared.last_seen.load(Ordering::SeqCst);
                    last_seen.store(state, Ordering::SeqCst);
                    if follow_active.load(Ordering::SeqCst)
                        && let Err(err) = write_state_file(&state_file, display_state_is_on(state))
                    {
                        crate::logging::append_line(
                            &log_path,
                            &format!(
                                "failed to write display.state ({}): {err}",
                                state_file.display()
                            ),
                        );
                    }
                }
                PowerSettingUnregisterNotification(registration);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn display_state_is_on_treats_only_zero_as_off() {
        assert!(!display_state_is_on(0));
        assert!(display_state_is_on(1));
        // dimmed still shows content → on
        assert!(display_state_is_on(2));
    }

    #[test]
    fn write_state_file_writes_on_and_off() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("display.state");

        write_state_file(&path, false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "off\n");
        assert!(std::fs::metadata(&path).is_ok());

        write_state_file(&path, true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "on\n");
    }

    #[test]
    fn effective_state_on_follows_mode_and_display_state() {
        // On / Off force the state regardless of the display power state
        assert!(effective_state_on(DisplayMode::On, 0));
        assert!(!effective_state_on(DisplayMode::Off, 1));
        // Follow mirrors the display power state (dimmed still counts as on)
        assert!(!effective_state_on(DisplayMode::Follow, 0));
        assert!(effective_state_on(DisplayMode::Follow, 1));
        assert!(effective_state_on(DisplayMode::Follow, 2));
    }

    #[test]
    fn heartbeat_state_on_force_off_while_suspended() {
        // While the machine is asleep the heartbeat must keep writing
        // "off" no matter the mode or display power state: asterctl has to
        // send CloseTFT before it is killed, and a mid-sleep heartbeat
        // must not flip the file back to "on".
        assert!(!heartbeat_state_on(DisplayMode::On, 1, true));
        assert!(!heartbeat_state_on(DisplayMode::Follow, 0, true));
        assert!(!heartbeat_state_on(DisplayMode::Follow, 1, true));
        // Awake: the mode's effective state applies as before.
        assert!(heartbeat_state_on(DisplayMode::On, 0, false));
        assert!(!heartbeat_state_on(DisplayMode::Off, 1, false));
        assert!(heartbeat_state_on(DisplayMode::Follow, 2, false));
    }

    #[test]
    fn display_mode_from_index_roundtrips_and_defaults() {
        for (mode, _) in crate::config::DISPLAY_OPTIONS {
            assert_eq!(DisplayMode::from_index(mode.index()), mode);
        }
        assert_eq!(DisplayMode::from_index(99), DisplayMode::On);
    }
}
