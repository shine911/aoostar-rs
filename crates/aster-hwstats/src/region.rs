// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! The named shared-memory region and its platform backends.

use std::fmt;

use crate::slot;
use crate::{Producer, REGION_NAME, REGION_SIZE, SlotSnapshot};

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win::Mapping;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix::Mapping;

/// Errors produced by the shared-memory region.
#[derive(Debug)]
pub enum Error {
    /// The OS could not create or open the named mapping.
    Os(String),
    /// The region is smaller than the protocol requires.
    BadRegion,
    /// The slot header version differs from this crate's [`crate::VERSION`].
    VersionMismatch(u32),
    /// The slot was written concurrently while reading; retry.
    Contended,
    /// The region name could not be encoded for the platform.
    InvalidName,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Os(e) => write!(f, "OS error: {e}"),
            Error::BadRegion => write!(f, "shared region smaller than required"),
            Error::VersionMismatch(v) => {
                write!(
                    f,
                    "slot version {v} does not match protocol version {}",
                    crate::VERSION
                )
            }
            Error::Contended => write!(f, "slot was written concurrently, retry"),
            Error::InvalidName => write!(f, "invalid region name"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub(crate) fn os(e: impl fmt::Display) -> Self {
        Error::Os(e.to_string())
    }
}

/// A handle to the `AOOSTAR_HW_STATS` shared-memory region.
///
/// The region is created on demand (create-or-open semantics, matching the
/// C# writer's `MemoryMappedFile.CreateOrOpen`) and stays alive as long as
/// at least one process holds a handle.
pub struct SharedStatsRegion {
    mapping: Mapping,
}

impl SharedStatsRegion {
    /// Opens (or creates) the default region [`REGION_NAME`].
    pub fn open() -> Result<Self, Error> {
        Self::open_named(REGION_NAME)
    }

    /// Opens (or creates) a region with a custom name. Intended for tests.
    pub fn open_named(name: &str) -> Result<Self, Error> {
        Ok(Self {
            mapping: Mapping::open(name, REGION_SIZE)?,
        })
    }

    /// Reads a consistent snapshot of the given producer's slot.
    ///
    /// Returns `Ok(None)` when the slot has not been written yet (producer
    /// not started). Use [`SlotSnapshot::sequence`] to detect new data.
    pub fn read_snapshot(&self, producer: Producer) -> Result<Option<SlotSnapshot>, Error> {
        slot::read_slot(self.mapping.as_slice(), producer.slot_offset())
    }

    /// Writes `payload` into the given producer's slot.
    ///
    /// `sequence` must be monotonically increasing by 2 across calls (the
    /// protocol uses odd/even seqlock markers); `timestamp_ms` from the Unix
    /// epoch clock. Returns the number of payload bytes written (truncated
    /// to the slot capacity). Currently used by tests; the production
    /// writers are `HwBridge.exe` (C#) and, in a later step, `aster-sysinfo`.
    pub fn write_payload(
        &mut self,
        producer: Producer,
        payload: &[u8],
        sequence: u64,
        timestamp_ms: u64,
    ) -> Result<usize, Error> {
        slot::write_slot(
            self.mapping.as_mut_slice(),
            producer.slot_offset(),
            payload,
            sequence,
            timestamp_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Unique region name per test run (parallel tests must not collide).
    fn unique_name(tag: &str) -> String {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("aoostar_hwstats_test_{}_{n}", tag)
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let name = unique_name("roundtrip");
        let mut region = SharedStatsRegion::open_named(&name).unwrap();
        assert!(matches!(
            region.read_snapshot(Producer::HwBridge).unwrap(),
            None
        ));

        let ts = now_ms();
        region
            .write_payload(Producer::HwBridge, b"cpu_temperature: 45.5\n", 2, ts)
            .unwrap();

        let snap = region.read_snapshot(Producer::HwBridge).unwrap().unwrap();
        assert_eq!(snap.sequence, 2);
        assert_eq!(snap.timestamp_ms, ts);
        assert_eq!(snap.payload, b"cpu_temperature: 45.5\n");

        // second write bumps the sequence
        region
            .write_payload(Producer::HwBridge, b"cpu_temperature: 45.6\n", 4, now_ms())
            .unwrap();
        let snap = region.read_snapshot(Producer::HwBridge).unwrap().unwrap();
        assert_eq!(snap.sequence, 4);
        assert_eq!(snap.payload, b"cpu_temperature: 45.6\n");
    }

    #[test]
    fn two_handles_share_the_region() {
        let name = unique_name("twohandles");
        let mut writer = SharedStatsRegion::open_named(&name).unwrap();
        writer
            .write_payload(Producer::HwBridge, b"gpu_core: 12\n", 2, now_ms())
            .unwrap();

        // second handle, opened afterwards, sees the same snapshot
        let reader = SharedStatsRegion::open_named(&name).unwrap();
        let snap = reader.read_snapshot(Producer::HwBridge).unwrap().unwrap();
        assert_eq!(snap.sequence, 2);
        assert_eq!(snap.payload, b"gpu_core: 12\n");
    }

    #[test]
    fn producer_slots_are_independent() {
        let name = unique_name("slots");
        let mut region = SharedStatsRegion::open_named(&name).unwrap();
        region
            .write_payload(Producer::HwBridge, b"hw: 1\n", 2, now_ms())
            .unwrap();

        // sysinfo slot untouched
        assert!(matches!(
            region.read_snapshot(Producer::SysInfo).unwrap(),
            None
        ));

        region
            .write_payload(Producer::SysInfo, b"sys: 2\n", 2, now_ms())
            .unwrap();
        let hw = region.read_snapshot(Producer::HwBridge).unwrap().unwrap();
        assert_eq!(hw.payload, b"hw: 1\n");
        let sys = region.read_snapshot(Producer::SysInfo).unwrap().unwrap();
        assert_eq!(sys.payload, b"sys: 2\n");
    }
}
