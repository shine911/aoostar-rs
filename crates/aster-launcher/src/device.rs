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

/// Why a device restart failed. `CONFIGRET` values are `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartFailure {
    /// Device not present / could not be located.
    NotPresent,
    /// `CM_Disable_DevNode` failed with this CONFIGRET.
    DisableFailed(u32),
    /// `CM_Enable_DevNode` failed with this CONFIGRET (after one retry).
    EnableFailed(u32),
}

/// Disables and re-enables the device with instance ID `instance` (CfgMgr32
/// equivalents of Device Manager's "Disable device" / "Enable device"),
/// forcing the USB stack to re-enumerate it. Requires Administrator (the
/// launcher runs elevated). A short delay between disable and enable lets
/// PnP finish processing the disable before the enable is attempted.
#[cfg(windows)]
pub(crate) fn restart_device(instance: &str) -> Result<(), RestartFailure> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Disable_DevNode, CM_Enable_DevNode, CM_Locate_DevNodeW, CR_SUCCESS,
    };

    unsafe {
        let id: Vec<u16> = format!("{instance}\0").encode_utf16().collect();
        let mut dev_inst: u32 = 0;
        if CM_Locate_DevNodeW(&mut dev_inst, id.as_ptr(), 0) != CR_SUCCESS {
            return Err(RestartFailure::NotPresent);
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
        Ok(())
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
pub(crate) fn restart_uart(vid: u16, pid: u16) -> Result<(), RestartFailure> {
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
