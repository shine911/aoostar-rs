// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! Shared-memory sensor source (`HwBridge.exe --shm` → `AOOSTAR_HW_STATS`).
//!
//! [`SharedMemoryProvider`] polls the HwBridge slot of the shared region and
//! merges new sensor values into the same `HashMap<String, String>` the file
//! slurper fills, so rendering and panel configs are unaffected. New data is
//! detected via the producer's sequence number; the timestamp is used to
//! warn about stale data (e.g. HwBridge frozen after sleep/resume).

use aster_hwstats::{Producer, REGION_NAME, SharedStatsRegion, parse_key_value};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Warn when the producer's timestamp is older than this. Covers the largest
/// allowed refresh interval (30 s) plus margin.
const STALE_AFTER_MS: u64 = 60_000;

/// A poll-based reader for the HwBridge slot of the shared memory region.
pub struct SharedMemoryProvider {
    region: Option<SharedStatsRegion>,
    last_sequence: u64,
    connected_logged: bool,
    stale_warned: bool,
}

impl SharedMemoryProvider {
    /// Creates a provider; the region is opened lazily on the first
    /// [`update`](Self::update) call.
    pub fn new() -> Self {
        Self {
            region: None,
            last_sequence: 0,
            connected_logged: false,
            stale_warned: false,
        }
    }

    /// Polls the shared region and merges new HwBridge values into `values`.
    ///
    /// Safe to call on every render iteration: it is a no-op unless the
    /// producer bumped its sequence since the last call.
    pub fn update(&mut self, values: &mut HashMap<String, String>) {
        if self.region.is_none() {
            match SharedStatsRegion::open() {
                Ok(region) => {
                    self.region = Some(region);
                    self.connected_logged = true;
                    info!("Connected to shared memory sensor region '{REGION_NAME}'");
                }
                Err(e) => {
                    if !self.connected_logged {
                        info!("Shared memory sensor region '{REGION_NAME}' not available: {e}");
                        self.connected_logged = true;
                    }
                    return;
                }
            }
        }

        let region = self.region.as_mut().expect("region initialized above");
        let snapshot = match region.read_snapshot(Producer::HwBridge) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                if !self.connected_logged {
                    info!("Shared memory sensor slot not written by HwBridge yet");
                    self.connected_logged = true;
                }
                return;
            }
            Err(e) => {
                warn!("Failed to read shared memory sensors: {e}");
                return;
            }
        };

        if snapshot.sequence != self.last_sequence {
            debug!(
                "Shared memory sensors updated (sequence {}): {} payload bytes",
                snapshot.sequence,
                snapshot.payload.len()
            );
            for (key, value) in parse_key_value(&snapshot.payload) {
                values.insert(key, value);
            }
            self.last_sequence = snapshot.sequence;
        }

        // Stale detection: HwBridge updates its timestamp on every write, so
        // an old timestamp means the producer stopped (e.g. after resume).
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let age_ms = now_ms.saturating_sub(snapshot.timestamp_ms);
        if age_ms > STALE_AFTER_MS {
            if !self.stale_warned {
                warn!(
                    "Shared memory sensors are stale: last update {age_ms}ms ago (sequence {}) — HwBridge may be stuck",
                    snapshot.sequence
                );
                self.stale_warned = true;
            }
        } else if self.stale_warned {
            info!("Shared memory sensors are fresh again");
            self.stale_warned = false;
        }
    }
}
