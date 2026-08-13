// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! Windows backend: a pagefile-backed named file mapping.
//!
//! Uses `CreateFileMappingW` with `INVALID_HANDLE_VALUE` (pagefile-backed,
//! exactly what C#'s `MemoryMappedFile.CreateOrOpen(name, capacity)` does),
//! so the C# writer in `hwbridge/HwBridge.cs` interoperates with zero extra
//! files. `CreateFileMappingW` transparently opens an existing mapping of the
//! same name, which gives us create-or-open semantics on both sides.

#![allow(non_snake_case)]

use std::io;
use std::os::raw::c_void;

use crate::Error;

type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_ALL_ACCESS: u32 = 0x000F_001F;

// SAFETY: declarations of the stable kernel32 API used below.
unsafe extern "system" {
    fn CreateFileMappingW(
        h_file: Handle,
        lp_attributes: *mut c_void,
        fl_protect: u32,
        dw_maximum_size_high: u32,
        dw_maximum_size_low: u32,
        lp_name: *const u16,
    ) -> Handle;
    fn MapViewOfFile(
        h_file_mapping_object: Handle,
        dw_desired_access: u32,
        dw_file_offset_high: u32,
        dw_file_offset_low: u32,
        dw_number_of_bytes_to_map: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(lp_base_address: *const c_void) -> i32;
    fn CloseHandle(h_object: Handle) -> i32;
}

/// A mapped view of the shared region.
pub struct Mapping {
    handle: Handle,
    ptr: *mut u8,
    len: usize,
}

// The mapping is valid across threads; access is serialized by the caller.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    pub fn open(name: &str, size: usize) -> Result<Self, Error> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let high = ((size as u64) >> 32) as u32;
        let low = (size as u64) as u32;

        // SAFETY: kernel32 functions with valid null-terminated UTF-16 name
        // and valid handle/pointer arguments.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null_mut(),
                PAGE_READWRITE,
                high,
                low,
                wide.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(Error::os(io::Error::last_os_error()));
        }

        // SAFETY: `handle` is a valid file mapping handle returned above.
        let ptr = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size) };
        if ptr.is_null() {
            let err = io::Error::last_os_error();
            // SAFETY: `handle` is still valid here.
            unsafe { CloseHandle(handle) };
            return Err(Error::os(err));
        }

        Ok(Self {
            handle,
            ptr: ptr.cast::<u8>(),
            len: size,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr`/`len` describe a valid mapping owned by `self`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr`/`len` describe a valid mapping exclusively borrowed
        // here (mutable borrow of `self`).
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: both handles are valid and owned by `self`; order matters
        // (unmap before close) and is guaranteed here.
        unsafe {
            UnmapViewOfFile(self.ptr.cast::<c_void>());
            CloseHandle(self.handle);
        }
    }
}
