// SPDX-License-Identifier: MIT OR Apache-2.0

//! Windows PnP device helpers: find and re-enumerate the AOOSTAR USB UART.
//! Used on wake when `restart_uart_on_resume` is enabled: the panel's USB
//! endpoint can wedge during Modern Standby (enumerated but not responding —
//! "The semaphore timeout period has expired" on writes) and a PnP
//! re-enumeration re-negotiates the device with the bus driver. The ladder
//! deliberately avoids the disable/enable Device Manager workaround, which
//! can leave the device in a "restart required" pending state that makes
//! Windows ask for a reboot after repeated sleep/wake cycles.

// `main.rs` denies `unsafe_code` crate-wide; this module is the narrow,
// deliberate exception — it is pure CfgMgr32 FFI glue. Scoped here so the
// crate-wide deny still guards every other module.
#![allow(unsafe_code)]

/// AOOSTAR LCD USB UART (mirrors `USB_UART_VID`/`USB_UART_PID` in
/// `asterctl-lcd`).
pub(crate) const AOOSTAR_UART_VID: u16 = 0x416;
pub(crate) const AOOSTAR_UART_PID: u16 = 0x90A1;

/// True if `instance` is an instance of the given USB VID/PID. Device
/// instance IDs look like `USB\VID_0416&PID_90A1\6&2a1b3c&0&1`.
pub(crate) fn is_our_instance(instance: &str, vid: u16, pid: u16) -> bool {
    let prefix = format!("USB\\VID_{vid:04X}&PID_{pid:04X}");
    let upper = instance.to_ascii_uppercase();
    // Anchor on the `\` after the prefix so only the device node itself
    // matches, not composite-device interface nodes (`&MI_xx`): restarting
    // the parent node re-enumerates all interfaces including the COM port.
    upper.starts_with(&prefix) && upper.as_bytes().get(prefix.len()) == Some(&b'\\')
}

/// Why a device re-enumeration failed. `CONFIGRET` values are `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartFailure {
    /// Device not present / could not be located.
    NotPresent,
    /// `CM_Reset_Device` failed with this CONFIGRET. Deliberately NOT
    /// followed by the other fallbacks: a failing function-level reset is a
    /// worse device state than no restart.
    ResetFailed(u32),
    /// `CM_Reenumerate_DevNode` failed with this CONFIGRET.
    ReenumerateFailed(u32),
    /// `CM_Query_And_Remove_SubTree` failed with this CONFIGRET.
    RemoveFailed(u32),
    /// `CM_Get_Parent` (locating the hub to rescan after removal) failed
    /// with this CONFIGRET.
    ParentLookupFailed(u32),
}

/// Which mechanism successfully re-enumerated the device (logged by
/// `power.rs` so the wake path is diagnosable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartMethod {
    /// `CM_Reset_Device` function-level reset (USB port reset): the device
    /// re-enumerates in place, no PnP state machine, no reboot ask.
    Reset,
    /// Remove the device subtree (unplug simulation) then re-enumerate its
    /// parent hub (replug simulation). The real recovery for the
    /// wake-from-Modern-Standby stale link; no reboot required.
    RemoveRescan,
}

/// `CM_Reset_Device` scope: reset the device instance itself.
#[cfg(windows)]
const CM_RESET_DEVICE_SCOPE_DEVICE: u32 = 0;

/// `CM_Reset_Device` — function-level reset of a device instance (for USB,
/// a port reset that re-enumerates the device at the same port). Not
/// exposed by windows-sys 0.60, so it is resolved dynamically via
/// `GetProcAddress`. Although documented as available since Windows 10
/// 1809, the export is empirically ABSENT from `CfgMgr32.dll` on some
/// builds (verified missing on Windows 10 25H2 build 26200), so the
/// dynamic lookup is the only reliable way to use it: `Err(None)` means
/// the export is unavailable (caller falls back to disable/enable),
/// `Err(Some(code))` means the reset was attempted and failed with
/// `code` (caller must NOT fall back to disable/enable).
#[cfg(windows)]
fn cm_reset_device(dev_inst: u32) -> Result<(), Option<u32>> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    unsafe {
        let lib_name: Vec<u16> = "CfgMgr32.dll\0".encode_utf16().collect();
        let lib = LoadLibraryW(lib_name.as_ptr());
        if lib.is_null() {
            return Err(None);
        }

        let fn_name = b"CM_Reset_Device\0";
        let proc = GetProcAddress(lib, fn_name.as_ptr());
        let Some(proc) = proc else {
            return Err(None);
        };

        type ResetFn = unsafe extern "system" fn(dev_inst: u32, scope: u32) -> u32;
        let reset: ResetFn = std::mem::transmute(proc);
        let ret = reset(dev_inst, CM_RESET_DEVICE_SCOPE_DEVICE);
        if ret == windows_sys::Win32::Devices::DeviceAndDriverInstallation::CR_SUCCESS {
            Ok(())
        } else {
            Err(Some(ret))
        }
    }
}

/// Re-enumerates the device with instance ID `instance` so a wedged USB
/// endpoint comes back (the wake-from-Modern-Standby fix). Ladder, in order:
///
/// 1. [`cm_reset_device`] — function-level USB port reset, in place, when
///    the export exists (dynamically resolved: it is absent from some
///    builds, verified on Windows 11 25H2 build 26200).
/// 2. Remove the device subtree (unplug simulation) then re-enumerate its
///    parent hub (replug simulation). This is the real recovery when the
///    panel's MCU power-cycles on wake (boot animation) but the host keeps
///    the stale link: a plain [`CM_Reenumerate_DevNode`] on the leaf node
///    does NOT tear the port down, so the stale endpoint survives it.
///
/// Step 2 deliberately replaces the old disable/enable workaround, which
/// leaves the device in a "restart required" pending state after repeated
/// cycles (Windows then demands a reboot to apply the change) — and a plain
/// re-enumerate, which looks successful but does not clear the stale link.
/// Requires Administrator (the launcher runs elevated).
#[cfg(windows)]
pub(crate) fn restart_device(instance: &str) -> Result<RestartMethod, RestartFailure> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Parent, CM_Locate_DevNodeW, CM_Query_And_Remove_SubTreeW, CM_REENUMERATE_NORMAL,
        CM_Reenumerate_DevNode, CR_SUCCESS,
    };

    unsafe {
        let id: Vec<u16> = format!("{instance}\0").encode_utf16().collect();
        let mut dev_inst: u32 = 0;
        if CM_Locate_DevNodeW(&mut dev_inst, id.as_ptr(), 0) != CR_SUCCESS {
            return Err(RestartFailure::NotPresent);
        }

        // 1) Preferred path: function-level reset.
        match cm_reset_device(dev_inst) {
            Ok(()) => return Ok(RestartMethod::Reset),
            // Reset API exists but failed: do NOT chain further
            // re-enumerations on a device mid-reset — worse state than
            // no restart.
            Err(Some(code)) => return Err(RestartFailure::ResetFailed(code)),
            // Export unavailable: fall through to the PnP ladder.
            Err(None) => {}
        }

        // 2) Unplug/replug simulation: remove the device subtree, then
        // re-enumerate its parent (the hub) so the device is rediscovered
        // from scratch — a real port teardown that clears the stale link.
        // A plain CM_Reenumerate_DevNode on the leaf node is NOT enough:
        // it returns success without tearing the port down, so the stale
        // endpoint survives it (observed on the GEM12).
        let remove_ret = CM_Query_And_Remove_SubTreeW(
            dev_inst,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            0,
        );
        if remove_ret != CR_SUCCESS {
            return Err(RestartFailure::RemoveFailed(remove_ret));
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let mut parent: u32 = 0;
        let parent_ret = CM_Get_Parent(&mut parent, dev_inst, 0);
        if parent_ret != CR_SUCCESS {
            return Err(RestartFailure::ParentLookupFailed(parent_ret));
        }
        let rescan_ret = CM_Reenumerate_DevNode(parent, CM_REENUMERATE_NORMAL);
        if rescan_ret != CR_SUCCESS {
            return Err(RestartFailure::ReenumerateFailed(rescan_ret));
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
        Ok(RestartMethod::RemoveRescan)
    }
}

/// Finds the instance ID of the first *present* USB device matching
/// `vid`/`pid`, or None.
#[cfg(windows)]
pub(crate) fn find_uart_instance(vid: u16, pid: u16) -> Option<String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_GETIDLIST_FILTER_PRESENT, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW,
        CR_SUCCESS,
    };

    unsafe {
        // Two-call pattern: query the buffer size, then the list.
        let mut size: u32 = 0;
        let filter: Vec<u16> = "USB\0".encode_utf16().collect();
        if CM_Get_Device_ID_List_SizeW(&mut size, filter.as_ptr(), 0) != CR_SUCCESS || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; size as usize];
        if CM_Get_Device_ID_ListW(
            filter.as_ptr(),
            buf.as_mut_ptr(),
            size,
            CM_GETIDLIST_FILTER_PRESENT,
        ) != CR_SUCCESS
        {
            return None;
        }

        // The list is NUL-separated instance IDs, terminated by a double NUL.
        let mut i = 0usize;
        while i < buf.len() {
            let start = i;
            while i < buf.len() && buf[i] != 0 {
                i += 1;
            }
            let id = String::from_utf16_lossy(&buf[start..i]);
            if id.is_empty() {
                break; // double NUL: end of list
            }
            if is_our_instance(&id, vid, pid) {
                return Some(id);
            }
            i += 1;
        }
        None
    }
}

/// Convenience: restart the AOOSTAR LCD UART by VID/PID, if present.
#[cfg(windows)]
pub(crate) fn restart_uart(vid: u16, pid: u16) -> Result<RestartMethod, RestartFailure> {
    match find_uart_instance(vid, pid) {
        Some(instance) => restart_device(&instance),
        None => Err(RestartFailure::NotPresent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_own_instance_id_case_insensitively() {
        assert!(is_our_instance(
            "USB\\VID_0416&PID_90A1\\6&2a1b3c&0&1",
            AOOSTAR_UART_VID,
            AOOSTAR_UART_PID
        ));
        assert!(is_our_instance(
            "usb\\vid_0416&pid_90a1\\6&2a1b3c&0&1",
            AOOSTAR_UART_VID,
            AOOSTAR_UART_PID
        ));
    }

    #[test]
    fn rejects_other_devices() {
        assert!(!is_our_instance(
            "USB\\VID_1234&PID_5678\\123",
            AOOSTAR_UART_VID,
            AOOSTAR_UART_PID
        ));
        assert!(!is_our_instance(
            "USB\\VID_0416&PID_90A2\\123",
            AOOSTAR_UART_VID,
            AOOSTAR_UART_PID
        ));
        assert!(!is_our_instance(
            "HID\\VID_0416&PID_90A1\\123",
            AOOSTAR_UART_VID,
            AOOSTAR_UART_PID
        ));
        // Composite interface nodes (&MI_xx) must not match: only the
        // parent device node is restarted.
        assert!(!is_our_instance(
            "USB\\VID_0416&PID_90A1&MI_00\\6&1",
            AOOSTAR_UART_VID,
            AOOSTAR_UART_PID
        ));
    }
}
