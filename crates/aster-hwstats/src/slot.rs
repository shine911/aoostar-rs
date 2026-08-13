// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! Slot header codec and seqlock-style snapshot reading.
//!
//! The functions here operate on plain byte slices so the protocol is fully
//! unit-testable without OS shared memory; [`crate::SharedStatsRegion`] hands
//! them the mapped bytes of the real region.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    Error, MAGIC, MAGIC_OFFSET, MAX_PAYLOAD, PAYLOAD_LEN_OFFSET, PAYLOAD_OFFSET, SEQUENCE_OFFSET,
    SLOT_SIZE, TIMESTAMP_OFFSET, VERSION, VERSION_OFFSET,
};

/// A consistent snapshot of one producer slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSnapshot {
    /// Writer sequence number; changes whenever a new snapshot was written.
    pub sequence: u64,
    /// Unix epoch milliseconds of the write (writer's clock).
    pub timestamp_ms: u64,
    /// The raw `label: value\n` payload bytes.
    pub payload: Vec<u8>,
}

/// Returns the byte range of the slot at `slot_offset`, `None` if the region
/// is smaller than the protocol requires.
pub(crate) fn slot_range(region_len: usize, slot_offset: usize) -> Option<std::ops::Range<usize>> {
    let end = slot_offset.checked_add(SLOT_SIZE)?;
    if end <= region_len {
        Some(slot_offset..end)
    } else {
        None
    }
}

/// Writes `payload` plus header into the slot using an odd/even seqlock.
///
/// `sequence` must be a monotonically increasing EVEN value: the slot is
/// first marked odd (write in progress), the payload is written, then the
/// sequence is stored again as the final even value. Readers never observe a
/// torn snapshot because they reject odd and racing sequence values.
///
/// Payloads longer than [`MAX_PAYLOAD`] are truncated. Returns the number of
/// payload bytes written.
pub(crate) fn write_slot(
    region: &mut [u8],
    slot_offset: usize,
    payload: &[u8],
    sequence: u64,
    timestamp_ms: u64,
) -> Result<usize, Error> {
    debug_assert_eq!(
        sequence % 2,
        0,
        "write_slot requires an even sequence value (odd/even seqlock)"
    );
    let slot = region
        .get_mut(slot_range(region.len(), slot_offset).ok_or(Error::BadRegion)?)
        .ok_or(Error::BadRegion)?;

    // SAFETY: `slot` is a valid, in-bounds mutable byte slice of at least
    // SEQUENCE_OFFSET + 8 bytes (SLOT_SIZE >= HEADER_SIZE).
    let seq_ptr = unsafe { slot.as_mut_ptr().add(SEQUENCE_OFFSET) } as *mut AtomicU64;
    debug_assert_eq!(seq_ptr as usize % 8, 0, "sequence must be 8-byte aligned");

    // 1. Mark the snapshot as in progress (odd). Release so it is visible
    //    before the payload writes below.
    // SAFETY: `seq_ptr` points into `slot`, which is 8-byte aligned and
    // exclusively borrowed here (no aliasing references are live).
    unsafe { (*seq_ptr).store(sequence - 1, Ordering::Release) };

    // 2. Payload + header fields.
    let n = payload.len().min(MAX_PAYLOAD);
    slot[PAYLOAD_OFFSET..PAYLOAD_OFFSET + n].copy_from_slice(&payload[..n]);
    write_u32(slot, PAYLOAD_LEN_OFFSET, n as u32);
    write_u64(slot, TIMESTAMP_OFFSET, timestamp_ms);
    write_u32(slot, MAGIC_OFFSET, MAGIC);
    write_u32(slot, VERSION_OFFSET, VERSION);

    // 3. Publish: store the final even value. Release ordering makes all
    //    payload writes above visible to readers that acquire this value.
    // SAFETY: same as step 1.
    unsafe { (*seq_ptr).store(sequence, Ordering::Release) };

    Ok(n)
}

/// Reads a consistent snapshot of the slot.
///
/// Returns `Ok(None)` if the slot was never written (magic absent). A
/// version mismatch or a write racing with the read (sequence changed between
/// the two atomic reads) is reported as an error; the caller should retry.
pub(crate) fn read_slot(region: &[u8], slot_offset: usize) -> Result<Option<SlotSnapshot>, Error> {
    let slot = region
        .get(slot_range(region.len(), slot_offset).ok_or(Error::BadRegion)?)
        .ok_or(Error::BadRegion)?;

    if read_u32(slot, MAGIC_OFFSET) != MAGIC {
        return Ok(None);
    }
    let version = read_u32(slot, VERSION_OFFSET);
    if version != VERSION {
        return Err(Error::VersionMismatch(version));
    }

    let seq_ptr = unsafe { slot.as_ptr().add(SEQUENCE_OFFSET) } as *const AtomicU64;
    debug_assert_eq!(seq_ptr as usize % 8, 0, "sequence must be 8-byte aligned");
    // SAFETY: `seq_ptr` points into `slot`, which is 8-byte aligned and
    // remains alive for the duration of this function (immutable borrow of
    // `region`).
    for _ in 0..4 {
        // An odd sequence means the writer is mid-update: retry.
        let seq1 = unsafe { (*seq_ptr).load(Ordering::Acquire) };
        if seq1 % 2 == 1 {
            continue;
        }
        let len = read_u32(slot, PAYLOAD_LEN_OFFSET) as usize;
        if len > MAX_PAYLOAD {
            // Torn read while the writer was mid-update: retry.
            continue;
        }
        let mut payload = vec![0u8; len];
        payload.copy_from_slice(&slot[PAYLOAD_OFFSET..PAYLOAD_OFFSET + len]);
        let timestamp_ms = read_u64(slot, TIMESTAMP_OFFSET);
        let seq2 = unsafe { (*seq_ptr).load(Ordering::Acquire) };
        // Accept only an unchanged EVEN sequence: any writer activity during
        // the read either left the sequence odd or bumped it, so seq1 != seq2.
        if seq1 == seq2 && seq2 % 2 == 0 {
            return Ok(Some(SlotSnapshot {
                sequence: seq1,
                timestamp_ms,
                payload,
            }));
        }
    }
    Err(Error::Contended)
}

fn read_u32(slot: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        slot[offset],
        slot[offset + 1],
        slot[offset + 2],
        slot[offset + 3],
    ])
}

fn read_u64(slot: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        slot[offset],
        slot[offset + 1],
        slot[offset + 2],
        slot[offset + 3],
        slot[offset + 4],
        slot[offset + 5],
        slot[offset + 6],
        slot[offset + 7],
    ])
}

fn write_u32(slot: &mut [u8], offset: usize, value: u32) {
    slot[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(slot: &mut [u8], offset: usize, value: u64) {
    slot[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{REGION_SIZE, SLOT_SIZE};

    fn region() -> Vec<u8> {
        vec![0u8; REGION_SIZE]
    }

    #[test]
    fn uninitialized_slot_reads_none() {
        let mut bytes = region();
        // slot 1 never written
        assert_eq!(read_slot(&bytes, SLOT_SIZE).unwrap(), None);
        // writing slot 0 must not touch slot 1
        write_slot(&mut bytes, 0, b"a: 1\n", 2, 1000).unwrap();
        assert_eq!(read_slot(&bytes, SLOT_SIZE).unwrap(), None);
    }

    #[test]
    fn write_read_roundtrip() {
        let mut bytes = region();
        let payload = b"cpu_temperature: 45.5\ngpu_core: 12\n";
        let n = write_slot(&mut bytes, 0, payload, 8, 1234).unwrap();
        assert_eq!(n, payload.len());

        let snap = read_slot(&bytes, 0).unwrap().unwrap();
        assert_eq!(snap.sequence, 8);
        assert_eq!(snap.timestamp_ms, 1234);
        assert_eq!(snap.payload, payload);
    }

    #[test]
    fn sequence_is_even_after_write_at_expected_offset() {
        let mut bytes = region();
        write_slot(&mut bytes, 0, b"k: v\n", 42, 99).unwrap();
        // header fields in the documented byte positions
        assert_eq!(read_u32(&bytes[..], 0), MAGIC);
        assert_eq!(read_u32(&bytes[..], 4), VERSION);
        assert_eq!(read_u64(&bytes[..], 8), 42); // sequence at offset 8
        assert_eq!(read_u64(&bytes[..], 16), 99); // timestamp at offset 16
        assert_eq!(read_u32(&bytes[..], 24), 5); // payload len "k: v\n"
    }

    #[test]
    fn overwrite_updates_snapshot() {
        let mut bytes = region();
        write_slot(&mut bytes, 0, b"a: 1\n", 2, 100).unwrap();
        write_slot(&mut bytes, 0, b"a: 2\n", 4, 200).unwrap();
        let snap = read_slot(&bytes, 0).unwrap().unwrap();
        assert_eq!(snap.sequence, 4);
        assert_eq!(snap.timestamp_ms, 200);
        assert_eq!(snap.payload, b"a: 2\n");
    }

    #[test]
    fn payload_is_truncated_to_slot_capacity() {
        let mut bytes = region();
        let big = vec![b'x'; MAX_PAYLOAD + 1000];
        let n = write_slot(&mut bytes, 0, &big, 2, 1).unwrap();
        assert_eq!(n, MAX_PAYLOAD);
        let snap = read_slot(&bytes, 0).unwrap().unwrap();
        assert_eq!(snap.payload.len(), MAX_PAYLOAD);
    }

    #[test]
    fn version_mismatch_is_reported() {
        let mut bytes = region();
        write_slot(&mut bytes, 0, b"a: 1\n", 2, 1).unwrap();
        write_u32(&mut bytes, VERSION_OFFSET, VERSION + 1);
        assert!(matches!(
            read_slot(&bytes, 0),
            Err(Error::VersionMismatch(v)) if v == VERSION + 1
        ));
    }

    #[test]
    fn odd_sequence_is_rejected_as_in_progress() {
        let mut bytes = region();
        write_slot(&mut bytes, 0, b"a: 1\n", 2, 1).unwrap();
        // simulate a writer stuck between the odd and even stores
        write_u64(&mut bytes, SEQUENCE_OFFSET, 3); // odd
        assert!(matches!(read_slot(&bytes, 0), Err(Error::Contended)));
    }

    #[test]
    fn slot_layout_matches_csharp_writer() {
        // Golden bytes: the exact header a writer must produce for a known
        // payload/sequence/timestamp. HwBridge.cs must match this layout.
        let mut bytes = region();
        write_slot(
            &mut bytes,
            0,
            b"cpu_temperature: 45.5\n",
            6,
            1_700_000_000_123,
        )
        .unwrap();
        let header: Vec<u8> = bytes[..32].to_vec();
        assert_eq!(
            header,
            vec![
                0x31, 0x53, 0x57, 0x48, // magic "HWS1" (LE)
                0x01, 0x00, 0x00, 0x00, // version 1
                0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // sequence 6
                0x7B, 0x68, 0xE5, 0xCF, 0x8B, 0x01, 0x00, 0x00, // 1700000000123 LE
                0x16, 0x00, 0x00, 0x00, // payload len 22
                0x00, 0x00, 0x00, 0x00, // reserved
            ]
        );
    }
}
