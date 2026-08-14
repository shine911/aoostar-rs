// SPDX-License-Identifier: MIT OR Apache-2.0

//! Windows PnP device helpers: find and power-cycle the AOOSTAR USB UART —
//! the automated version of the manual "disable in Device Manager,
//! re-enable" workaround that was previously required after every sleep.

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

/// Why a device reset failed. `CONFIGRET` values are `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartFailure {
    /// Device not present / could not be located.
    NotPresent,
    /// `CM_Reset_Device` failed with this CONFIGRET. Deliberately NOT
    /// followed by a disable/enable fallback: a failing function-level
    /// reset is a worse device state than no restart, and disable/enable is
    /// what can leave the device in the "restart required" pending state.
    ResetFailed(u32),
    /// `CM_Disable_DevNode` failed with this CONFIGRET.
    DisableFailed(u32),
    /// `CM_Enable_DevNode` failed with this CONFIGRET (after one retry).
    EnableFailed(u32),
}

/// Which mechanism successfully re-enumerated the device (logged by
/// `power.rs` so the wake path is diagnosable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartMethod {
    /// `CM_Reset_Device` function-level reset (USB port reset): the device
    /// re-enumerates in place, without the disable→enable state machine, so
    /// Windows never asks for a reboot.
    Reset,
    /// `CM_Reset_Device` is not available (pre-1809 Windows); the old
    /// disable/enable sequence was used instead.
    DisableEnable,
}

/// `CM_Reset_Device` scope: reset the device instance itself.
#[cfg(windows)]
const CM_RESET_DEVICE_SCOPE_DEVICE: u32 = 0;

/// `CM_Reset_Device` — function-level reset of a device instance (for USB,
/// a port reset that re-enumerates the device at the same port). Not
/// exposed by windows-sys 0.60 and only present on Windows 10 1809+, so it
/// is resolved dynamically via `GetProcAddress`; `Err(None)` means the
/// export is unavailable (caller falls back to disable/enable),
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

/// Re-enumerates the device with instance ID `instance`. Preferred path is
/// the function-level reset [`cm_reset_device`] (in-place USB port reset —
/// no pending "restart required" state). On pre-1809 Windows, where that
/// API does not exist, it falls back to the CfgMgr32 equivalents of Device
/// Manager's "Disable device" / "Enable device" (the old behavior).
/// Requires Administrator (the launcher runs elevated).
#[cfg(windows)]
pub(crate) fn restart_device(instance: &str) -> Result<RestartMethod, RestartFailure> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Disable_DevNode, CM_Enable_DevNode, CM_Locate_DevNodeW, CR_SUCCESS,
    };

    unsafe {
        let id: Vec<u16> = format!("{instance}\0").encode_utf16().collect();
        let mut dev_inst: u32 = 0;
        if CM_Locate_DevNodeW(&mut dev_inst, id.as_ptr(), 0) != CR_SUCCESS {
            return Err(RestartFailure::NotPresent);
        }

        // Preferred path: function-level reset.
        match cm_reset_device(dev_inst) {
            Ok(()) => return Ok(RestartMethod::Reset),
            // Reset API exists but failed: do NOT fall back to
            // disable/enable, which can leave the device in the
            // "restart required" state that Windows wants a reboot for.
            Err(Some(code)) => return Err(RestartFailure::ResetFailed(code)),
            // Pre-1809 Windows: no CM_Reset_Device export — disable/enable.
            Err(None) => {}
        }

        let ret = CM_Disable_DevNode(dev_inst, 0);
        if ret != CR_SUCCESS {
            return Err(RestartFailure::DisableFailed(ret));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        let ret = CM_Enable_DevNode(dev_inst, 0);
        if ret != CR_SUCCESS {
            // PnP contention is transient: retry once before giving up.
            std::thread::sleep(std::time::Duration::from_millis(250));
            let retry = CM_Enable_DevNode(dev_inst, 0);
            if retry != CR_SUCCESS {
                return Err(RestartFailure::EnableFailed(retry));
            }
        }
        Ok(RestartMethod::DisableEnable)
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
