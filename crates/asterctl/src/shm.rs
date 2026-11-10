// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! Shared-memory sensor sources (`HwBridge.exe --shm` and
//! `aster-sysinfo --shm` → `AOOSTAR_HW_STATS`).
//!
//! [`SharedMemoryProvider`] polls both producer slots of the shared region
//! and merges new sensor values into the same `HashMap<String, String>` the
//! file slurper fills, so rendering and panel configs are unaffected. New
//! data is detected via each producer's sequence number
//! ([`SharedStatsRegion::read_if_changed`] checks it before copying the
//! payload, so an idle poll allocates nothing); timestamps are used to warn
//! about stale data (e.g. a producer frozen after sleep/resume).

use aster_hwstats::{Error, Producer, REGION_NAME, SharedStatsRegion, SlotUpdate, parse_key_value};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// The producers whose slots this provider reads.
const PRODUCERS: [Producer; 2] = [Producer::HwBridge, Producer::SysInfo];

/// Warn when a producer's timestamp is older than this. Covers the largest
/// allowed refresh interval (30 s) plus margin.
const STALE_AFTER_MS: u64 = 60_000;

fn producer_index(producer: Producer) -> usize {
    match producer {
        Producer::HwBridge => 0,
        Producer::SysInfo => 1,
    }
}

fn producer_name(producer: Producer) -> &'static str {
    match producer {
        Producer::HwBridge => "HwBridge",
        Producer::SysInfo => "aster-sysinfo",
    }
}

/// A poll-based reader for the HwBridge and SysInfo slots of the shared
/// memory region.
pub struct SharedMemoryProvider {
    region: Option<SharedStatsRegion>,
    /// Sequence of the last snapshot merged per producer (`None`: nothing
    /// read yet, so the next read always copies).
    last_sequences: [Option<u64>; 2],
    /// Writer timestamp of that snapshot. It only changes together with the
    /// sequence, so it stays valid for the stale check while the slot is
    /// unchanged.
    last_timestamps: [Option<u64>; 2],
    connected_logged: [bool; 2],
    stale_warned: [bool; 2],
}

impl SharedMemoryProvider {
    /// Creates a provider; the region is opened lazily on the first
    /// [`update`](Self::update) call.
    pub fn new() -> Self {
        Self {
            region: None,
            last_sequences: [None; 2],
            last_timestamps: [None; 2],
            connected_logged: [false; 2],
            stale_warned: [false; 2],
        }
    }

    /// Polls the shared region and merges new values into `values`.
    ///
    /// Safe to call on every render iteration: it is a no-op unless a
    /// producer bumped its sequence since the last call.
    pub fn update(&mut self, values: &mut HashMap<String, String>) {
        if self.region.is_none() {
            match SharedStatsRegion::open() {
                Ok(region) => {
                    self.region = Some(region);
                    info!("Connected to shared memory sensor region '{REGION_NAME}'");
                }
                Err(e) => {
                    if !self.connected_logged[0] {
                        info!("Shared memory sensor region '{REGION_NAME}' not available: {e}");
                        self.connected_logged[0] = true;
                    }
                    return;
                }
            }
        }

        let region = self.region.as_ref().expect("region initialized above");
        for producer in PRODUCERS {
            let idx = producer_index(producer);
            match region.read_if_changed(producer, self.last_sequences[idx]) {
                Ok(SlotUpdate::Changed(snapshot)) => {
                    debug!(
                        "Shared memory sensors updated ({} slot, sequence {}): {} payload bytes",
                        producer_name(producer),
                        snapshot.sequence,
                        snapshot.payload.len()
                    );
                    for (key, value) in parse_key_value(&snapshot.payload) {
                        values.insert(key, value);
                    }
                    self.last_sequences[idx] = Some(snapshot.sequence);
                    self.last_timestamps[idx] = Some(snapshot.timestamp_ms);
                    self.connected_logged[idx] = false;
                }
                Ok(SlotUpdate::Unchanged) => {}
                Ok(SlotUpdate::Empty) => {
                    if !self.connected_logged[idx] {
                        info!(
                            "Shared memory sensor slot not written by {} yet",
                            producer_name(producer)
                        );
                        self.connected_logged[idx] = true;
                    }
                    continue;
                }
                Err(Error::Contended) => {
                    // The producer was mid-write for every retry: routine
                    // at high poll rates; the next poll picks it up.
                    debug!(
                        "Shared memory sensors busy ({} slot), retrying next poll",
                        producer_name(producer)
                    );
                    continue;
                }
                Err(e) => {
                    warn!(
                        "Failed to read shared memory sensors ({}): {e}",
                        producer_name(producer)
                    );
                    continue;
                }
            }
            let Some(timestamp_ms) = self.last_timestamps[idx] else {
                continue;
            };

            // Stale detection: producers update their timestamp on every
            // write, so an old timestamp means they stopped (e.g. after
            // resume).
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let age_ms = now_ms.saturating_sub(timestamp_ms);
            if age_ms > STALE_AFTER_MS {
                if !self.stale_warned[idx] {
                    warn!(
                        "Shared memory sensors are stale ({} slot): last update {age_ms}ms ago (sequence {}) — producer may be stuck",
                        producer_name(producer),
                        self.last_sequences[idx].unwrap_or_default()
                    );
                    self.stale_warned[idx] = true;
                }
            } else if self.stale_warned[idx] {
                info!(
                    "Shared memory sensors are fresh again ({} slot)",
                    producer_name(producer)
                );
                self.stale_warned[idx] = false;
            }
        }
    }
}
