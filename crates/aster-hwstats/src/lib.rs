// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

#![forbid(non_ascii_idents)]
#![warn(missing_docs)]

//! Shared-memory hardware stats protocol for the AOOSTAR sensor pipeline.
//!
//! Replaces the `label: value` text-file handoff (`cfg/sensors/*.txt`) with a
//! tiny in-memory snapshot bus:
//!
//! ```text
//!   HwBridge.exe (C#, writer)  ──┐
//!                                ├──►  AOOSTAR_HW_STATS  ──►  asterctl (Rust, reader)
//!   aster-sysinfo (Rust, writer) ─┘        (shared memory)
//! ```
//!
//! The region is a fixed-size byte buffer with two fixed-size producer slots
//! (HwBridge = slot 0, aster-sysinfo = slot 1). Each slot carries a small
//! header plus a UTF-8 `label: value\n` payload (the same format the file
//! reader used), so panels and sensor labels keep working unchanged.
//!
//! # Slot layout (little-endian, offsets relative to the slot start)
//!
//! ```text
//!   0  magic        u32   = MAGIC ("HWS1"), 0 = slot never written
//!   4  version      u32   = VERSION
//!   8  sequence     u64   = writer counter: EVEN = valid snapshot,
//!                           ODD = write in progress. Bumped to odd before
//!                           the payload is written and to even after
//!                           (seqlock; readers reject odd/racing snapshots)
//!  16  timestamp_ms u64   = Unix epoch milliseconds of this snapshot
//!  24  payload_len  u32   = payload byte length (<= MAX_PAYLOAD)
//!  28  reserved     u32   = 0
//!  32  payload      bytes = "label: value\n..." (UTF-8)
//! ```
//!
//! Readers detect new data by comparing `sequence` to the previously seen
//! value; the odd/even protocol plus a double-read of `sequence` guarantee a
//! consistent snapshot even while the writer is mid-update. `timestamp_ms`
//! is used only to detect stale data (e.g. a producer frozen after
//! sleep/resume).

pub mod payload;

mod region;
mod slot;

pub use payload::parse_key_value;
pub use region::{Error, SharedStatsRegion};
pub use slot::SlotSnapshot;

// === Protocol constants (must match hwbridge/HwBridge.cs) ===

/// Name of the shared memory region (Windows named file mapping / POSIX shm).
pub const REGION_NAME: &str = "AOOSTAR_HW_STATS";

/// Total region size: two producer slots.
pub const REGION_SIZE: usize = 2 * SLOT_SIZE;

/// Size of one producer slot.
pub const SLOT_SIZE: usize = 64 * 1024;

/// Size of the slot header (offsets below).
pub const HEADER_SIZE: usize = 32;

/// Maximum payload length in bytes per slot.
pub const MAX_PAYLOAD: usize = SLOT_SIZE - HEADER_SIZE;

/// Magic value marking an initialized slot (`b"HWS1"`).
pub const MAGIC: u32 = 0x4857_5331;

/// Protocol version; bumped on incompatible layout changes.
pub const VERSION: u32 = 1;

// Header field offsets (see module docs).
pub const MAGIC_OFFSET: usize = 0;
pub const VERSION_OFFSET: usize = 4;
pub const SEQUENCE_OFFSET: usize = 8;
pub const TIMESTAMP_OFFSET: usize = 16;
pub const PAYLOAD_LEN_OFFSET: usize = 24;
pub const PAYLOAD_OFFSET: usize = 32;

/// Identifies one producer; each owns a fixed slot in the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Producer {
    /// `HwBridge.exe` — LHM temperatures/loads (slot 0).
    HwBridge,
    /// `aster-sysinfo` — native CPU/mem/net/disk stats (slot 1, reserved).
    SysInfo,
}

impl Producer {
    /// Byte offset of this producer's slot inside the shared region.
    pub const fn slot_offset(self) -> usize {
        match self {
            Producer::HwBridge => 0,
            Producer::SysInfo => SLOT_SIZE,
        }
    }
}
