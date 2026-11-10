// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]

use clap::Parser;
use env_logger::Env;
use log::{debug, error, info};
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::io::{BufWriter, Write};
#[cfg(target_os = "linux")]
use std::net::IpAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
#[cfg(target_os = "linux")]
use std::str::FromStr;
use std::thread::sleep;
use std::time::{Duration, Instant};
#[cfg(not(windows))]
use sysinfo::Components;
use sysinfo::{
    CpuRefreshKind, DiskKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind,
    ProcessesToUpdate, RefreshKind, System,
};
use tempfile::Builder;

/// Cadence of the slow refresh group (disk list/usage, process count, Linux
/// per-disk storage sensors) when `--disk-refresh` is not given.
const DEFAULT_SLOW_REFRESH: Duration = Duration::from_secs(30);

/// Decides when the slow refresh group is due again.
#[derive(Debug)]
struct SlowCadence {
    interval: Duration,
    last: Option<Instant>,
}

impl SlowCadence {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// Due on the first call and then whenever `interval` has elapsed since
    /// the last [`Self::mark`].
    fn is_due(&self, now: Instant) -> bool {
        match self.last {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= self.interval,
        }
    }

    fn mark(&mut self, now: Instant) {
        self.last = Some(now);
    }
}

/// Allowed `--refresh` intervals, in seconds.
const REFRESH_OPTIONS: [u16; 4] = [2, 5, 10, 30];

/// Parses a `--refresh` value, accepting only [`REFRESH_OPTIONS`].
fn parse_refresh(value: &str) -> Result<u16, String> {
    let secs: u16 = value.parse().map_err(|_| {
        format!("invalid refresh interval '{value}': must be one of {REFRESH_OPTIONS:?}")
    })?;
    if REFRESH_OPTIONS.contains(&secs) {
        Ok(secs)
    } else {
        Err(format!(
            "invalid refresh interval '{value}': must be one of {REFRESH_OPTIONS:?}"
        ))
    }
}

/// Proof of concept sensor value collection for the asterctl screen control tool.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Output sensor file.
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Temporary directory for preparing the output sensor file.
    ///
    /// The system temp directory is used if not specified.
    /// The temp directory must be on the same file system for atomic rename operation!
    #[arg(short, long)]
    temp_dir: Option<PathBuf>,

    /// Write sensor values into the "AOOSTAR_HW_STATS" shared memory region
    /// (SysInfo slot) instead of the `--out` file. Used by the launcher on
    /// Windows; `asterctl --shm` reads the values directly, with no file I/O.
    #[arg(long)]
    shm: bool,

    /// Print values in console
    #[arg(long)]
    console: bool,

    /// System sensor refresh interval in seconds (one of 2, 5, 10, 30)
    #[arg(short, long, value_parser = parse_refresh)]
    refresh: Option<u16>,

    /// Enable individual disk refresh logic as used in AOOSTAR-X. Refresh interval in seconds.
    #[arg(long)]
    disk_refresh: Option<u16>,

    /// Retrieve drive temperature if `disk-update` option is enabled.
    ///
    /// Requires smartctl and password-less sudo!
    #[cfg(target_os = "linux")]
    #[arg(long)]
    smartctl: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    #[cfg(target_os = "linux")]
    let use_smartctl = args.smartctl;
    #[cfg(not(target_os = "linux"))]
    let use_smartctl = false;

    if let Some(out_file) = &args.out
        && let Some(parent) = out_file.parent()
    {
        fs::create_dir_all(parent)?;
    }
    // Fast group: rebuilt from scratch every tick, so vanished interfaces or
    // components stop being published.
    let mut sensors: HashMap<String, String> = HashMap::with_capacity(64);
    // Slow group: rebuilt on the slow cadence and republished every tick.
    let mut slow_sensors: HashMap<String, String> = HashMap::with_capacity(32);
    let mut storage_state = StorageState::default();
    let mut sysinfo_source = SysinfoSource::new();

    // Shared-memory writer (SysInfo slot), used instead of the output file
    // when --shm is passed (the launcher's default on Windows).
    let mut shm_writer = if args.shm {
        info!(
            "Writing sensors to shared memory region '{}'",
            aster_hwstats::REGION_NAME
        );
        Some(SharedMemoryWriter::new()?)
    } else {
        None
    };

    let refresh = Duration::from_secs(args.refresh.unwrap_or_default() as u64);

    let disk_refresh = Duration::from_secs(args.disk_refresh.unwrap_or_default() as u64);
    let mut slow_cadence = SlowCadence::new(if disk_refresh.is_zero() {
        DEFAULT_SLOW_REFRESH
    } else {
        disk_refresh
    });

    if !refresh.is_zero() {
        info!(
            "Starting aster-sysinfo with refresh={}ms",
            refresh.as_millis()
        );
    }

    loop {
        let upd_start_time = Instant::now();

        let slow_due = slow_cadence.is_due(upd_start_time);
        sysinfo_source.refresh(slow_due);
        sensors.clear();
        sysinfo_source.update_fast_sensors(&mut sensors);

        if slow_due {
            debug!("Refreshing slow sensor group (disks, processes)");
            slow_sensors.clear();
            sysinfo_source.update_slow_sensors(&mut slow_sensors);
            if !disk_refresh.is_zero() {
                update_linux_storage_sensors(&mut slow_sensors, use_smartctl, &mut storage_state);
            }
            slow_cadence.mark(upd_start_time);
        }
        sensors.extend(slow_sensors.iter().map(|(k, v)| (k.clone(), v.clone())));

        if let Some(writer) = shm_writer.as_mut() {
            writer.write(&sensors)?;
        } else if let Some(out_file) = &args.out {
            write_sensor_file(out_file, args.temp_dir.as_deref(), &sensors)?;
        }

        if args.console {
            // pretty print console output with sorted keys
            let mut lines: Vec<_> = sensors.iter().collect();
            lines.sort();
            for (label, value) in lines {
                println!("{}: {}", label, value);
            }
            println!();
        }

        if refresh.is_zero() {
            break;
        }

        let elapsed = upd_start_time.elapsed();
        if refresh > elapsed {
            sleep(refresh - elapsed);
        }
    }

    Ok(())
}

fn write_sensor_file(
    out_file: &Path,
    temp_dir: Option<&Path>,
    sensors: &HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if out_file.is_dir() {
        error!("Output cannot be a directory: {}", out_file.display());
        exit(1);
    }

    // make sure our sensor file can be read by everyone (POSIX only -- Windows
    // uses a different, ACL-based permission model, so there's nothing
    // equivalent to set here; the file is already readable by the user that
    // created it, which is sufficient for a single-user Windows setup)
    let mut builder = Builder::new();
    #[cfg(unix)]
    builder.permissions(fs::Permissions::from_mode(0o664));

    let tmp_file = if let Some(temp_path) = temp_dir {
        fs::create_dir_all(temp_path)?;

        debug!("Creating a new named temp file in {temp_path:?}");
        builder.tempfile_in(temp_path)?
    } else {
        debug!("Creating a new named temp file");
        builder.tempfile()?
    };

    debug!("Writing sensor temp file...");
    let mut stream = BufWriter::new(&tmp_file);

    for (label, value) in sensors.iter() {
        writeln!(stream, "{label}: {value}")?;
    }

    stream.flush()?;
    drop(stream);
    debug!("Renaming temp file to: {out_file:?}");
    tmp_file.persist(out_file)?;

    Ok(())
}

/// Serializes the sensor map into the `label: value\n` payload bytes used by
/// both the shared-memory slots and the legacy text file.
fn serialize_payload(sensors: &HashMap<String, String>) -> Vec<u8> {
    let mut payload = Vec::with_capacity(sensors.len() * 24);
    for (label, value) in sensors {
        payload.extend_from_slice(label.as_bytes());
        payload.extend_from_slice(b": ");
        payload.extend_from_slice(value.as_bytes());
        payload.push(b'\n');
    }
    payload
}

/// First sequence base for a writer: the previous slot sequence rounded up to
/// even (an odd value means a write was in flight), or 0 for an empty slot.
/// `write` adds 2 before every publish, so the next emitted sequence is
/// always strictly greater than anything a reader may have seen.
fn seed_sequence(previous: Option<u64>) -> u64 {
    previous.map_or(0, |s| s.saturating_add(s & 1))
}

/// Publishes the sensor map into the SysInfo slot of the `AOOSTAR_HW_STATS`
/// shared memory region, mirroring `HwBridge.exe --shm` (same protocol, same
/// odd/even sequence pattern).
struct SharedMemoryWriter {
    region: aster_hwstats::SharedStatsRegion,
    sequence: u64,
}

impl SharedMemoryWriter {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self::from_region(aster_hwstats::SharedStatsRegion::open()?))
    }

    /// Seeds the sequence from the SysInfo slot already in the region, so a
    /// restarted writer never re-emits a sequence a reader has already seen.
    fn from_region(region: aster_hwstats::SharedStatsRegion) -> Self {
        let previous = match region.read_snapshot(aster_hwstats::Producer::SysInfo) {
            Ok(snapshot) => snapshot.map(|s| s.sequence),
            Err(e) => {
                debug!("Unable to read previous SysInfo slot sequence: {e}");
                None
            }
        };
        Self {
            region,
            sequence: seed_sequence(previous),
        }
    }

    fn write(
        &mut self,
        sensors: &HashMap<String, String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.sequence += 2;
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let payload = serialize_payload(sensors);
        let n = self.region.write_payload(
            aster_hwstats::Producer::SysInfo,
            &payload,
            self.sequence,
            timestamp_ms,
        )?;
        debug!(
            "Wrote {} sensor values ({} bytes) to SysInfo slot, sequence {}",
            sensors.len(),
            n,
            self.sequence
        );
        Ok(())
    }
}

pub struct SysinfoSource {
    sys: System,
    disks: Disks,
    // Windows temperatures come from HwBridge; nothing consumes sysinfo's
    // components there, and enumerating them costs a WMI round-trip.
    #[cfg(not(windows))]
    components: Components,
    networks: Networks,
    /// Static host metadata (`system_name`, ...), computed once.
    static_info: Vec<(&'static str, String)>,
    total_processes: usize,
    last_refresh: Option<Instant>,
    refresh_duration: Option<Duration>,
}

impl Default for SysinfoSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SysinfoSource {
    pub fn new() -> Self {
        Self {
            // Only CPU (usage + frequency) and RAM/swap are needed per tick;
            // processes are never refreshed wholesale (see `refresh`).
            sys: System::new_with_specifics(
                RefreshKind::nothing()
                    .with_cpu(CpuRefreshKind::everything())
                    .with_memory(MemoryRefreshKind::everything()),
            ),
            disks: Disks::new(),
            #[cfg(not(windows))]
            components: Components::new(),
            networks: Networks::new(),
            static_info: collect_static_info(),
            total_processes: 0,
            last_refresh: None,
            refresh_duration: None,
        }
    }

    /// Refreshes the per-tick data (CPU, memory, components, networks).
    /// With `slow` also the disk list/usage and the process count.
    pub fn refresh(&mut self, slow: bool) {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        debug!("Refreshing components, networks");
        // `true` drops entries that vanished (unplugged disk, removed NIC).
        #[cfg(not(windows))]
        self.components.refresh(true);
        self.networks.refresh(true);
        if slow {
            debug!("Refreshing disks, process count");
            self.disks.refresh(true);
            // Only the count is published, so no per-process data is read.
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            self.total_processes = self.sys.processes().len();
        }

        if let Some(last_refresh) = self.last_refresh {
            self.refresh_duration = Some(last_refresh.elapsed());
        }
        self.last_refresh = Some(Instant::now());
    }

    fn update_fast_sensors(&self, sensors: &mut HashMap<String, String>) {
        debug!("Refreshing sensors");
        for cpu in self.sys.cpus() {
            add_sensor(
                sensors,
                format!("cpu_{}_frequency", cpu.name()),
                cpu.frequency(),
            );
            add_sensor(
                sensors,
                format!("cpu_{}_usage", cpu.name()),
                format!("{:.2}", cpu.cpu_usage()),
            );
        }

        add_sensor(
            sensors,
            "cpu_usage_percent".to_string(),
            format!("{:.2}", self.sys.global_cpu_usage()),
        );

        let load_avg = System::load_average();
        add_sensor(sensors, "load_avg_one", format!("{:.2}", load_avg.one));
        add_sensor(sensors, "load_avg_five", format!("{:.2}", load_avg.five));
        add_sensor(
            sensors,
            "load_avg_fifteen",
            format!("{:.2}", load_avg.fifteen),
        );

        // RAM and swap information:
        add_sensor(sensors, "mem_free_bytes", self.sys.free_memory());
        add_sensor(sensors, "mem_free", format_bytes(self.sys.free_memory()));
        add_sensor(sensors, "mem_total_bytes", self.sys.total_memory());
        add_sensor(sensors, "mem_total", format_bytes(self.sys.total_memory()));
        add_sensor(sensors, "mem_used_bytes", self.sys.used_memory());
        add_sensor(sensors, "mem_used", format_bytes(self.sys.used_memory()));
        add_sensor(
            sensors,
            "mem_usage_percent",
            format!(
                "{:.1}",
                (self.sys.used_memory() * 100) as f64 / self.sys.total_memory() as f64
            ),
        );

        add_sensor(sensors, "swap_free_bytes", self.sys.free_swap());
        add_sensor(sensors, "swap_free", format_bytes(self.sys.free_swap()));
        add_sensor(sensors, "swap_total_bytes", self.sys.total_swap());
        add_sensor(sensors, "swap_total", format_bytes(self.sys.total_swap()));
        add_sensor(sensors, "swap_used_bytes", self.sys.used_swap());
        add_sensor(sensors, "swap_used", format_bytes(self.sys.used_swap()));
        add_sensor(
            sensors,
            "swap_usage_percent",
            format!(
                "{:.1}",
                (self.sys.used_swap() * 100) as f64 / self.sys.total_swap() as f64
            ),
        );

        // System information:
        let up_secs = System::uptime();
        let up_days = up_secs / 86400;
        let up_hours = (up_secs - (up_days * 86400)) / 3600;
        let up_mins = (up_secs - (up_days * 86400) - (up_hours * 3600)) / 60;
        add_sensor(sensors, "system_uptime_sec", up_secs);
        /*
        Time to look into ftl for i18n
        The coreutils project did a lot of work that could be used:
        https://github.com/uutils/coreutils/blob/main/src/uucore/src/lib/mods/locale.rs
        Then this would be the easy way to format the time, just uses a lot of setup code:

        uptime-format = { $days ->
            [0] { $time }
            [one] { $days } day, { $time }
           *[other] { $days } days { $time }
        }

        translate!(
            "uptime-format",
            "days" => up_days,
            "time" => format!("{up_hours:02}:{up_mins:02}")
        )
         */
        let day_string = match up_days {
            0 => "",
            1 => "1 day, ",
            n => &format!("{n} days "),
        };
        add_sensor(
            sensors,
            "system_uptime",
            format!("{day_string}{up_hours:02}:{up_mins:02}"),
        );

        add_sensor(sensors, "cpu_count", self.sys.cpus().len());

        self.update_component_sensors(sensors);
        self.update_network_sensors(sensors);
    }

    /// Sensors that change slowly: static host metadata, process count, disks.
    fn update_slow_sensors(&self, sensors: &mut HashMap<String, String>) {
        for (label, value) in &self.static_info {
            add_sensor(sensors, *label, value);
        }
        add_sensor(sensors, "total_processes", self.total_processes);

        // disks' information:
        let mut ssd_idx = 0;
        let mut hdd_idx = 0;
        for disk in &self.disks {
            let label;
            match disk.kind() {
                DiskKind::SSD => {
                    label = format!("storage_ssd[{}]", ssd_idx);
                    ssd_idx += 1;
                }
                DiskKind::HDD => {
                    label = format!("storage_hdd[{}]", hdd_idx);
                    hdd_idx += 1;
                }
                _ => continue,
            }
            // special label for AOOSTAR-X system panel
            add_sensor(
                sensors,
                format!("{label}_usage_percent"),
                (disk.total_space().saturating_sub(disk.available_space()) * 100)
                    .checked_div(disk.total_space())
                    .unwrap_or(0),
            );

            // using similar labels as AOOSTAR-X, but combining `{label2}_{label}`
            let device = disk.name().to_string_lossy().replace(' ', "_");
            add_sensor(
                sensors,
                format!("disk_{device}_total_bytes"),
                disk.total_space(),
            );
            add_sensor(
                sensors,
                format!("disk_{device}_total"),
                format_bytes(disk.total_space()),
            );
            let used = disk.total_space().saturating_sub(disk.available_space());
            add_sensor(sensors, format!("disk_{device}_used_bytes"), used);
            add_sensor(sensors, format!("disk_{device}_used"), format_bytes(used));
            add_sensor(
                sensors,
                format!("disk_{device}_free_bytes"),
                disk.available_space(),
            );
            add_sensor(
                sensors,
                format!("disk_{device}_free"),
                format_bytes(disk.available_space()),
            );
            add_sensor(
                sensors,
                format!("disk_{device}_usage_percent"),
                format!(
                    "{:.1}",
                    disk.total_space().saturating_sub(disk.available_space()) as f64 * 100.0
                        / disk.total_space() as f64
                ),
            );
        }
    }

    #[cfg(windows)]
    fn update_component_sensors(&self, _sensors: &mut HashMap<String, String>) {}

    #[cfg(not(windows))]
    fn update_component_sensors(&self, sensors: &mut HashMap<String, String>) {
        // Components temperature:
        for component in &self.components {
            if let Some(temperature) = component.temperature() {
                let label;
                if component.label().contains("spd5118") {
                    label = "temperature_memory".to_string();
                } else if component.label().contains("amdgpu") {
                    label = "temperature_gpu".to_string();
                } else if component.label().contains("Tctl") {
                    label = "temperature_cpu".to_string();
                } else if component.label().contains("Composite")
                    && !component.label().contains("nvme")
                {
                    // just a guess...
                    label = "temperature_motherboard".to_string();
                } else {
                    label = format!("temperature_{}", component.label().replace(' ', "_"));
                    // println!("label={}, type_id={:?}, id={:?}, {component:?}",
                    //          component.label(), component.type_id(), component.id());
                }

                add_sensor(sensors, format!("{label}#unit"), "°C");
                let value = format!("{temperature:.1}");
                add_sensor(sensors, &label, &value);
                #[cfg(target_os = "linux")]
                match label.as_str() {
                    // Monitor3 draws these widgets' units itself. Keep the
                    // ordinary labels (and their #unit values) available for
                    // generic consumers, while exposing value-only aliases
                    // for the Linux mapping used by the built-in panel.
                    "temperature_cpu" => add_sensor(sensors, "temperature_cpu_value", &value),
                    "temperature_memory" => add_sensor(sensors, "temperature_memory_value", &value),
                    "temperature_gpu" => add_sensor(sensors, "temperature_gpu_value", &value),
                    _ => {}
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Some(gpu_busy) = read_gpu_busy_percent(Path::new("/sys/class/drm")) {
                add_sensor(sensors, "gpu_core#unit", "%");
                add_sensor(sensors, "gpu_core", gpu_busy);
            }
        }
    }

    fn update_network_sensors(&self, sensors: &mut HashMap<String, String>) {
        // Network interfaces name, total data received and total data transmitted:
        #[cfg(target_os = "linux")]
        let mut network_candidates = Vec::new();
        for (interface_name, data) in &self.networks {
            // only consider specific interfaces
            // NOTE: the original prefix list ("eth", "en", "em", "wlan", "wlp", "wlo") only
            // matches Linux/BSD interface names (e.g. eth0, wlan0, wlp2s0). On Windows the
            // sysinfo crate reports the adapter's friendly name instead (e.g. "Ethernet",
            // "Wi-Fi"), so "Wi-Fi" never matched any prefix and was silently dropped --
            // added "wi-fi"/"wifi"/"wireless" to also cover Windows Wi-Fi adapters.
            #[cfg(not(target_os = "linux"))]
            {
                let if_name = interface_name.to_lowercase();
                if ![
                    "eth", "en", "em", "wlan", "wlp", "wlo", "wi-fi", "wifi", "wireless",
                ]
                .iter()
                .any(|i| if_name.starts_with(*i))
                {
                    continue;
                }
            }
            #[cfg(target_os = "linux")]
            {
                let mut addresses: Vec<String> = data
                    .ip_networks()
                    .iter()
                    .map(|network| network.addr.to_string())
                    .collect();
                addresses.sort();
                network_candidates.push(NetworkCandidate {
                    name: interface_name.to_string(),
                    addresses,
                    received: data.received(),
                    transmitted: data.transmitted(),
                });
                continue;
            }
            // Sort by address to avoid random order in refreshes
            #[allow(unreachable_code)]
            let mut addrs: Vec<_> = data.ip_networks().iter().map(|net| net.addr).collect();
            #[allow(unreachable_code)]
            addrs.sort();
            #[allow(unreachable_code)]
            for (idx, addr) in addrs.into_iter().enumerate() {
                add_sensor(
                    sensors,
                    format!("network_{interface_name}_address{idx}"),
                    addr,
                );
            }

            if let Some(refresh) = self.refresh_duration {
                let interval = refresh.as_millis() as u64;
                if interval > 0 {
                    add_sensor(
                        sensors,
                        format!("network_{interface_name}_download_speed"),
                        format!("{}/s", format_bytes(1000 * data.received() / interval)),
                    );
                    add_sensor(
                        sensors,
                        format!("network_{interface_name}_upload_speed"),
                        format!("{}/s", format_bytes(1000 * data.transmitted() / interval)),
                    );
                }
            }

            add_sensor(
                sensors,
                format!("network_{interface_name}_total_received_bytes"),
                data.total_received(),
            );
            add_sensor(
                sensors,
                format!("network_{interface_name}_total_received"),
                format_bytes(data.total_received()),
            );
            add_sensor(
                sensors,
                format!("network_{interface_name}_total_transmitted_bytes"),
                data.total_transmitted(),
            );
            add_sensor(
                sensors,
                format!("network_{interface_name}_total_transmitted"),
                format_bytes(data.total_transmitted()),
            );
        }

        #[cfg(target_os = "linux")]
        {
            if let Some(network) = select_network_candidate(&network_candidates) {
                add_sensor(sensors, "network_interface_name", &network.name);
                if let Some(address) = network
                    .addresses
                    .iter()
                    .filter_map(|address| IpAddr::from_str(address).ok())
                    .find(|address| !address.is_loopback())
                {
                    add_sensor(sensors, "network_ip_address", address);
                }
                if let Some(refresh) = self.refresh_duration {
                    let interval = refresh.as_millis() as u64;
                    if interval > 0 {
                        add_sensor(
                            sensors,
                            "network_download_speed",
                            format!("{}/s", format_bytes(1000 * network.received / interval)),
                        );
                        add_sensor(
                            sensors,
                            "network_upload_speed",
                            format!("{}/s", format_bytes(1000 * network.transmitted / interval)),
                        );
                    }
                }
            }
        }
    }
}

/// Host metadata that cannot change while the process runs.
fn collect_static_info() -> Vec<(&'static str, String)> {
    [
        ("system_name", System::name()),
        ("system_kernel_version", System::kernel_version()),
        ("system_os_version", System::os_version()),
        ("system_hostname", System::host_name()),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.map(|v| (label, v)))
    .collect()
}

fn add_sensor(
    sensors: &mut HashMap<String, String>,
    label: impl Into<String>,
    value: impl Display,
) {
    sensors.insert(label.into(), value.to_string());
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct NetworkCandidate {
    name: String,
    addresses: Vec<String>,
    received: u64,
    transmitted: u64,
}

#[cfg(target_os = "linux")]
fn select_network_candidate(candidates: &[NetworkCandidate]) -> Option<NetworkCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            candidate
                .addresses
                .iter()
                .filter_map(|address| IpAddr::from_str(address).ok())
                .any(|address| !address.is_loopback())
        })
        .max_by(|left, right| {
            let left_traffic = left.received.saturating_add(left.transmitted);
            let right_traffic = right.received.saturating_add(right.transmitted);
            (
                is_common_uplink_name(&left.name),
                left_traffic > 0,
                left_traffic,
            )
                .cmp(&(
                    is_common_uplink_name(&right.name),
                    right_traffic > 0,
                    right_traffic,
                ))
                // A lexical tie-break makes selection stable despite the
                // order in which sysinfo reports interfaces.
                .then_with(|| right.name.cmp(&left.name))
        })
        .cloned()
}

#[cfg(target_os = "linux")]
fn is_common_uplink_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["eth", "en", "em", "wlan", "wlp", "wlo", "ww"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

#[cfg(target_os = "linux")]
fn read_gpu_busy_percent(root: &Path) -> Option<String> {
    let mut entries: Vec<_> = fs::read_dir(root).ok()?.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let busy_path = entry.path().join("device/gpu_busy_percent");
        let Ok(value) = fs::read_to_string(busy_path) else {
            continue;
        };
        let value = value.trim();
        let Ok(percent) = value.parse::<u8>() else {
            continue;
        };
        if percent <= 100 {
            return Some(percent.to_string());
        }
    }
    None
}

/// State carried between slow refreshes of the Linux storage sensors.
#[derive(Debug, Default)]
struct StorageState {
    /// Last known smartctl temperature per device (kept while it sleeps).
    last_temperatures: HashMap<String, i32>,
}

fn update_linux_storage_sensors(
    sensors: &mut HashMap<String, String>,
    use_smartctl: bool,
    state: &mut StorageState,
) {
    // One `df` run serves every device.
    let mounts = read_df_mounts();

    // Note: AOOSTAR-X only considered spinning Rust. Too bad if you're using SSDs in the HD bays...
    if let Ok(hdd_devices) = get_storage_devices(StorageDevice::HddOrSsd) {
        debug!("HDD devices : {:?}", hdd_devices);
        for (idx, device) in hdd_devices.iter().enumerate() {
            add_storage_sensors(sensors, "hdd", idx, device, &mounts, use_smartctl, state);
        }
    }

    // AOOSTAR-X: ssd == nvme
    if let Ok(nvme_devices) = get_storage_devices(StorageDevice::Nvme) {
        debug!("NVME devices: {:?}", nvme_devices);
        for (idx, device) in nvme_devices.iter().enumerate() {
            add_storage_sensors(sensors, "ssd", idx, device, &mounts, use_smartctl, state);
        }
    }
}

fn add_storage_sensors(
    sensors: &mut HashMap<String, String>,
    kind: &str,
    idx: usize,
    device: &str,
    mounts: &[DfMount],
    use_smartctl: bool,
    state: &mut StorageState,
) {
    let usage = disk_usage_from_mounts(device, mounts);
    let prefix = format!("storage_{kind}[{idx}]");
    add_sensor(
        sensors,
        format!("{prefix}_total_size_bytes"),
        usage.total_size,
    );
    add_sensor(
        sensors,
        format!("{prefix}_total_size"),
        format_bytes(usage.total_size),
    );
    add_sensor(
        sensors,
        format!("{prefix}_total_used_bytes"),
        usage.total_used,
    );
    add_sensor(
        sensors,
        format!("{prefix}_total_used"),
        format_bytes(usage.total_used),
    );
    add_sensor(
        sensors,
        format!("{prefix}_usage_percent"),
        usage.usage_percent,
    );

    if use_smartctl {
        match get_smartctl_disk_temperature(device) {
            SmartctlTemperature::Value(t) => {
                state.last_temperatures.insert(device.to_string(), t);
            }
            // Asleep: keep the last value instead of waking the drive.
            SmartctlTemperature::Standby => {}
            SmartctlTemperature::Unavailable => {
                state.last_temperatures.remove(device);
            }
        }
        if let Some(t) = state.last_temperatures.get(device) {
            add_sensor(sensors, format!("{prefix}_temperature"), t);
        }
    }
}

#[derive(Debug)]
pub struct DiskInfo {
    pub device: String,
    pub temperature: i32,
    pub used: f64,
    pub total_used: u64,
    pub total_size: u64,
}

#[derive(Debug)]
pub struct DiskUsage {
    pub usage_percent: f64,
    pub total_used: u64,
    pub total_size: u64,
}

#[derive(Debug, PartialEq)]
pub enum StorageDevice {
    All,
    Hdd,
    Ssd,
    HddOrSsd,
    Nvme,
}

pub type DiskResult = Result<Vec<DiskInfo>, Box<dyn std::error::Error>>;

/// Get storage devices of the given type: NVME, SSD or HD
///
/// Storage devices are identified from /sys/block attributes.
/// Removable devices are excluded.
///
/// # Arguments
///
/// * `kind`: type of storage device
///
/// returns: sorted list of found device names (`sd*` and `nvme*`)
pub fn get_storage_devices(kind: StorageDevice) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut devices = Vec::new();
    let sys_block = Path::new("/sys/block");

    if !sys_block.exists() {
        info!("No storage device found");
        return Ok(devices);
    }

    for entry in fs::read_dir(sys_block)? {
        let entry = entry?;
        let dev_name = entry.file_name();
        let dev_str = dev_name.to_string_lossy();

        // filter out all non sd* and nvme* devices
        let is_nvme = is_nvme_device_name(&dev_str);
        let is_storage = is_sd_device_name(&dev_str);
        if !(is_nvme || is_storage) {
            continue;
        }

        match kind {
            StorageDevice::All => {}
            StorageDevice::Hdd | StorageDevice::Ssd | StorageDevice::HddOrSsd => {
                if !is_storage {
                    continue;
                }
            }
            StorageDevice::Nvme => {
                if !is_nvme {
                    continue;
                }
            }
        };

        if is_nvme {
            let dev_name = entry.file_name();
            let dev_str = dev_name.to_string_lossy();
            devices.push(dev_str.to_string());
            continue;
        }

        let rotational_path = sys_block.join(dev_str.as_ref()).join("queue/rotational");
        let removable_path = sys_block.join(dev_str.as_ref()).join("removable");

        match (
            fs::read_to_string(&rotational_path),
            fs::read_to_string(&removable_path),
        ) {
            (Ok(rotational), Ok(removable)) => {
                let rotational = rotational.trim();
                let removable = removable.trim();

                // ignore removable
                if removable == "1" {
                    continue;
                }

                if kind == StorageDevice::Hdd && rotational == "1"
                    || kind == StorageDevice::Ssd && rotational == "0"
                    || kind == StorageDevice::HddOrSsd
                {
                    devices.push(dev_str.to_string());
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                error!("Unable to read device {dev_str} attributes: {e}");
            }
        }
    }

    devices.sort();
    Ok(devices)
}

/// Outcome of a smartctl temperature query.
#[derive(Debug, PartialEq, Eq)]
pub enum SmartctlTemperature {
    Value(i32),
    /// Device is in a low-power mode and was deliberately not woken up.
    Standby,
    Unavailable,
}

/// Retrieve temperature from NVMe or SDD/HDD with smartctl.
///
/// `-n standby` makes smartctl skip drives that are spun down (exit status 2)
/// instead of waking them for every poll.
pub fn get_smartctl_disk_temperature(dev: &str) -> SmartctlTemperature {
    let dev = format!("/dev/{}", dev);
    match Command::new("sudo")
        .args(["-n", "smartctl", "-n", "standby", "-A"])
        .arg(&dev)
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            parse_smartctl_output(output.status.code(), &stdout)
        }
        Err(e) => {
            error!("Device {dev} acquisition failed, error: {e}");
            SmartctlTemperature::Unavailable
        }
    }
}

fn parse_smartctl_output(exit_code: Option<i32>, stdout: &str) -> SmartctlTemperature {
    // smartctl prints "Device is in STANDBY mode, exit(2)" for `-n standby`.
    if exit_code == Some(2) && stdout.contains("Device is in") {
        return SmartctlTemperature::Standby;
    }
    match parse_smartctl_temperature(stdout) {
        Some(t) => SmartctlTemperature::Value(t),
        None => SmartctlTemperature::Unavailable,
    }
}

/// Extracts the temperature from `smartctl -A` output: the SATA attribute
/// `194 Temperature_Celsius ... - <raw>` first, then the NVMe
/// `Temperature: <n> Celsius` line.
fn parse_smartctl_temperature(stdout: &str) -> Option<i32> {
    let sata = stdout.lines().find_map(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        // ID NAME FLAG VALUE WORST THRESH TYPE UPDATED WHEN_FAILED RAW
        if tokens.len() > 9 && tokens[0] == "194" && tokens[1] == "Temperature_Celsius" {
            if tokens[8] != "-" {
                return None;
            }
            let digits: String = tokens[9].chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<i32>().ok()
        } else {
            None
        }
    });
    sata.or_else(|| {
        stdout.lines().find_map(|line| {
            let rest = line.split_once("Temperature:")?.1;
            let mut tokens = rest.split_whitespace();
            let value = tokens.next()?.parse::<i32>().ok()?;
            // the value must be followed by a unit
            tokens.next()?;
            Some(value)
        })
    })
}

/// A mounted filesystem as reported by `df`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfMount {
    pub source: String,
    pub size: u64,
    pub used: u64,
}

/// Runs `df` once for all filesystems (no shell involved).
fn read_df_mounts() -> Vec<DfMount> {
    match Command::new("df")
        .args(["--block-size=1", "--output=source,size,used"])
        .output()
    {
        // df exits non-zero when some filesystem is inaccessible but still
        // prints the rest, so the status is deliberately not checked.
        Ok(output) => parse_df_output(&String::from_utf8_lossy(&output.stdout)),
        Err(e) => {
            error!("Unable to run df: {e}");
            Vec::new()
        }
    }
}

/// Parses `df --output=source,size,used` lines; the header is skipped because
/// its size/used columns are not numeric.
fn parse_df_output(stdout: &str) -> Vec<DfMount> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace().rev();
            let used = tokens.next()?.parse::<u64>().ok()?;
            let size = tokens.next()?.parse::<u64>().ok()?;
            let source = tokens.rev().collect::<Vec<_>>().join(" ");
            (!source.is_empty()).then_some(DfMount { source, size, used })
        })
        .collect()
}

/// Whether `/dev/...` `source` is the block device `dev` itself or one of its
/// partitions: `sda` -> `sda`, `sda1`; `nvme0n1` -> `nvme0n1`, `nvme0n1p1`.
/// Unlike a prefix match, `sda` does not match `sdab1`.
fn is_device_or_partition(source: &str, dev: &str) -> bool {
    let Some(name) = source.strip_prefix("/dev/") else {
        return false;
    };
    let Some(rest) = name.strip_prefix(dev) else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    let partition = if is_nvme_device_name(dev) {
        rest.strip_prefix('p')
    } else {
        Some(rest)
    };
    partition.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// `sd` followed by one or more letters (`sda`, `sdab`).
fn is_sd_device_name(name: &str) -> bool {
    name.strip_prefix("sd")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase()))
}

/// `nvme<N>n<M>` (`nvme0n1`).
fn is_nvme_device_name(name: &str) -> bool {
    let Some((ctrl, ns)) = name.strip_prefix("nvme").and_then(|r| r.split_once('n')) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(ctrl) && digits(ns)
}

/// Sums size/used over all filesystems mounted from `dev` or its partitions.
fn disk_usage_from_mounts(dev: &str, mounts: &[DfMount]) -> DiskUsage {
    let mut usage = DiskUsage {
        usage_percent: 0.0,
        total_used: 0,
        total_size: 0,
    };
    for mount in mounts
        .iter()
        .filter(|m| is_device_or_partition(&m.source, dev))
    {
        usage.total_size += mount.size;
        usage.total_used += mount.used;
    }
    if usage.total_size != 0 {
        usage.usage_percent =
            ((usage.total_used as f64 / usage.total_size as f64) * 100.0 * 100.0).round() / 100.0;
    }
    usage
}

/// Format bytes into human-readable string
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    const THRESHOLD: f64 = 1024.0;

    if bytes == 0 {
        return "0 B".to_string();
    }

    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= THRESHOLD && unit_index < UNITS.len() - 1 {
        size /= THRESHOLD;
        unit_index += 1;
    }

    if unit_index > 0 {
        format!("{:.2} {}", size, UNITS[unit_index])
    } else {
        format!("{} {}", size, UNITS[unit_index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1048576), "1.00 MB");
        assert_eq!(format_bytes(1073741824), "1.00 GB");
    }

    #[test]
    fn test_parse_refresh_allows_only_configured_intervals() {
        assert_eq!(parse_refresh("2"), Ok(2));
        assert_eq!(parse_refresh("5"), Ok(5));
        assert_eq!(parse_refresh("10"), Ok(10));
        assert_eq!(parse_refresh("30"), Ok(30));
        assert!(parse_refresh("3").is_err());
        assert!(parse_refresh("0").is_err());
        assert!(parse_refresh("abc").is_err());
        assert!(parse_refresh("").is_err());
    }

    #[test]
    fn test_serialize_payload_roundtrips_through_parser() {
        let mut sensors = HashMap::new();
        sensors.insert("cpu_usage_percent".to_string(), "12.3".to_string());
        sensors.insert("mem_usage_percent".to_string(), "45.6".to_string());
        sensors.insert(
            "network_Wi-Fi_download_speed".to_string(),
            "1048576".to_string(),
        );

        let payload = serialize_payload(&sensors);
        let parsed = aster_hwstats::parse_key_value(&payload);

        assert_eq!(parsed.len(), sensors.len());
        for (label, value) in &sensors {
            assert_eq!(parsed.get(label).unwrap(), value);
        }
    }

    #[test]
    fn slow_cadence_is_due_first_then_after_interval() {
        let mut c = SlowCadence::new(Duration::from_secs(30));
        let t0 = Instant::now();
        assert!(c.is_due(t0));
        c.mark(t0);
        assert!(!c.is_due(t0 + Duration::from_secs(2)));
        assert!(!c.is_due(t0 + Duration::from_secs(29)));
        assert!(c.is_due(t0 + Duration::from_secs(30)));
        // clock going backwards must not be due
        assert!(!c.is_due(t0));
    }

    #[test]
    fn seed_sequence_rounds_up_to_even() {
        assert_eq!(seed_sequence(None), 0);
        assert_eq!(seed_sequence(Some(0)), 0);
        assert_eq!(seed_sequence(Some(10)), 10);
        assert_eq!(seed_sequence(Some(11)), 12);
        assert_eq!(seed_sequence(Some(u64::MAX)), u64::MAX);
    }

    #[test]
    fn writer_resumes_after_previous_sequence() {
        let name = format!("aster-sysinfo-test-{}", std::process::id());
        let mut first = aster_hwstats::SharedStatsRegion::open_named(&name).unwrap();
        first
            .write_payload(aster_hwstats::Producer::SysInfo, b"a: 1\n", 42, 1)
            .unwrap();
        let second = aster_hwstats::SharedStatsRegion::open_named(&name).unwrap();
        let mut writer = SharedMemoryWriter::from_region(second);
        assert_eq!(writer.sequence, 42);
        writer.write(&HashMap::new()).unwrap();
        assert_eq!(writer.sequence, 44);
    }

    #[test]
    fn partition_matching_is_exact() {
        assert!(is_device_or_partition("/dev/sda", "sda"));
        assert!(is_device_or_partition("/dev/sda1", "sda"));
        assert!(is_device_or_partition("/dev/sda12", "sda"));
        assert!(!is_device_or_partition("/dev/sdab1", "sda"));
        assert!(!is_device_or_partition("/dev/sdab", "sda"));
        assert!(!is_device_or_partition("/dev/sdb1", "sda"));
        assert!(!is_device_or_partition("sda1", "sda"));
        assert!(is_device_or_partition("/dev/nvme0n1", "nvme0n1"));
        assert!(is_device_or_partition("/dev/nvme0n1p2", "nvme0n1"));
        assert!(!is_device_or_partition("/dev/nvme0n11p2", "nvme0n1"));
        assert!(!is_device_or_partition("/dev/nvme0n12", "nvme0n1"));
    }

    #[test]
    fn device_name_checks() {
        assert!(is_sd_device_name("sda"));
        assert!(is_sd_device_name("sdab"));
        assert!(!is_sd_device_name("sd"));
        assert!(!is_sd_device_name("sda1"));
        assert!(is_nvme_device_name("nvme0n1"));
        assert!(is_nvme_device_name("nvme10n12"));
        assert!(!is_nvme_device_name("nvme0n1p1"));
        assert!(!is_nvme_device_name("nvme0"));
    }

    #[test]
    fn df_output_sums_partitions_only() {
        let out = "Filesystem 1B-blocks Used\n\
                   /dev/sda1 1000 250\n\
                   /dev/sda2 3000 750\n\
                   /dev/sdab1 5000 5000\n\
                   tmpfs 10 1\n";
        let mounts = parse_df_output(out);
        assert_eq!(mounts.len(), 4);
        let usage = disk_usage_from_mounts("sda", &mounts);
        assert_eq!(usage.total_size, 4000);
        assert_eq!(usage.total_used, 1000);
        assert_eq!(usage.usage_percent, 25.0);
        let none = disk_usage_from_mounts("sdc", &mounts);
        assert_eq!(none.total_size, 0);
        assert_eq!(none.usage_percent, 0.0);
    }

    #[test]
    fn smartctl_parsing() {
        let sata = "ID# ATTRIBUTE_NAME FLAG VALUE WORST THRESH TYPE UPDATED WHEN_FAILED RAW_VALUE\n\
                    194 Temperature_Celsius     0x0022   036   045   000    Old_age   Always       -       36 (Min/Max 20/45)\n";
        assert_eq!(
            parse_smartctl_output(Some(0), sata),
            SmartctlTemperature::Value(36)
        );
        let nvme = "Temperature Sensor 1:               40 Celsius\nTemperature:                        38 Celsius\n";
        assert_eq!(
            parse_smartctl_output(Some(0), nvme),
            SmartctlTemperature::Value(38)
        );
        assert_eq!(
            parse_smartctl_output(Some(2), "Device is in STANDBY mode, exit(2)\n"),
            SmartctlTemperature::Standby
        );
        assert_eq!(
            parse_smartctl_output(Some(2), ""),
            SmartctlTemperature::Unavailable
        );
        assert_eq!(
            parse_smartctl_output(Some(0), "garbage"),
            SmartctlTemperature::Unavailable
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn network_alias_selection_prefers_traffic_and_is_name_stable() {
        let candidates = vec![
            NetworkCandidate {
                name: "Meta".into(),
                addresses: vec!["192.0.2.20".into()],
                received: 100_000,
                transmitted: 100_000,
            },
            NetworkCandidate {
                name: "tun0".into(),
                addresses: vec!["100.64.0.1".into()],
                received: 90_000,
                transmitted: 90_000,
            },
            NetworkCandidate {
                name: "docker0".into(),
                addresses: vec!["172.17.0.1".into()],
                received: 80_000,
                transmitted: 80_000,
            },
            NetworkCandidate {
                name: "wlp4s0".into(),
                addresses: vec!["192.0.2.10".into()],
                received: 1,
                transmitted: 1,
            },
        ];
        assert_eq!(
            select_network_candidate(&candidates).unwrap().name,
            "wlp4s0"
        );

        let tied = vec![
            NetworkCandidate {
                name: "zeta".into(),
                addresses: vec!["198.51.100.2".into()],
                received: 1,
                transmitted: 1,
            },
            NetworkCandidate {
                name: "alpha".into(),
                addresses: vec!["198.51.100.3".into()],
                received: 1,
                transmitted: 1,
            },
        ];
        assert_eq!(select_network_candidate(&tied).unwrap().name, "alpha");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn network_alias_falls_back_to_virtual_interface_when_needed() {
        let candidates = vec![
            NetworkCandidate {
                name: "docker0".into(),
                addresses: vec!["172.17.0.1".into()],
                received: 5,
                transmitted: 5,
            },
            NetworkCandidate {
                name: "tun0".into(),
                addresses: vec!["100.64.0.1".into()],
                received: 5,
                transmitted: 5,
            },
        ];
        assert_eq!(
            select_network_candidate(&candidates).unwrap().name,
            "docker0"
        );
        assert!(
            select_network_candidate(&[NetworkCandidate {
                name: "lo".into(),
                addresses: vec!["127.0.0.1".into()],
                received: 100,
                transmitted: 100,
            }])
            .is_none()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gpu_busy_percent_reads_valid_sysfs_fixture_only() {
        let dir = tempfile::tempdir().unwrap();
        let device = dir.path().join("card0/device");
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("gpu_busy_percent"), "42\n").unwrap();
        assert_eq!(read_gpu_busy_percent(dir.path()), Some("42".into()));

        fs::write(device.join("gpu_busy_percent"), "101\n").unwrap();
        assert_eq!(read_gpu_busy_percent(dir.path()), None);
        fs::write(device.join("gpu_busy_percent"), "not-a-number\n").unwrap();
        assert_eq!(read_gpu_busy_percent(dir.path()), None);
    }
}
