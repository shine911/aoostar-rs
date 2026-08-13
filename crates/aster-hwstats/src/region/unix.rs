// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! POSIX backend: a named shared-memory object (`shm_open`/`mmap`).
//!
//! Used to exercise the protocol with real OS shared memory in unit tests on
//! Linux (the production path is Windows). The creator unlinks the name on
//! drop; openers of an existing object do not.

use std::ffi::CString;
use std::io;

use crate::Error;

use libc::{MAP_FAILED, MAP_SHARED, PROT_READ, PROT_WRITE};

/// A mapped view of the shared region.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
    name: CString,
    unlink_on_drop: bool,
}

// The mapping is valid across threads; access is serialized by the caller.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    pub fn open(name: &str, size: usize) -> Result<Self, Error> {
        let mut full = String::with_capacity(name.len() + 1);
        full.push('/');
        full.push_str(name);
        let cname = CString::new(full).map_err(|_| Error::InvalidName)?;

        // Try to create first, so we know whether to unlink on drop; fall
        // back to opening when the object already exists.
        let mut fd = unsafe {
            libc::shm_open(
                cname.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        let mut unlink_on_drop = true;
        if fd < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EEXIST) {
                return Err(Error::os(err));
            }
            fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0) };
            if fd < 0 {
                return Err(Error::os(io::Error::last_os_error()));
            }
            unlink_on_drop = false;
        }

        if unsafe { libc::ftruncate(fd, size as libc::off_t) } != 0 {
            let err = io::Error::last_os_error();
            // SAFETY: `fd` is open.
            unsafe { libc::close(fd) };
            if unlink_on_drop {
                // SAFETY: `cname` is a valid C string.
                unsafe { libc::shm_unlink(cname.as_ptr()) };
            }
            return Err(Error::os(err));
        }

        // SAFETY: `fd` is a valid shm descriptor with `size` bytes.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };
        // SAFETY: `fd` is open.
        unsafe { libc::close(fd) };

        if ptr == MAP_FAILED {
            let err = io::Error::last_os_error();
            if unlink_on_drop {
                // SAFETY: `cname` is a valid C string.
                unsafe { libc::shm_unlink(cname.as_ptr()) };
            }
            return Err(Error::os(err));
        }

        Ok(Self {
            ptr: ptr.cast::<u8>(),
            len: size,
            name: cname,
            unlink_on_drop,
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
        // SAFETY: `ptr` is a valid mapping returned by mmap.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
        if self.unlink_on_drop {
            // SAFETY: `name` is a valid C string.
            unsafe { libc::shm_unlink(self.name.as_ptr()) };
        }
    }
}
