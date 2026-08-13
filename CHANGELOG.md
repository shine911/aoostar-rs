# AOOSTAR WTR MAX Screen Control Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

_Changes in the next release_

### Added
- New `aster-hwstats` crate: shared-memory hardware stats protocol (Windows named file mapping
  `AOOSTAR_HW_STATS`, two producer slots with per-slot `magic`/`version`/`sequence`/`timestamp` header and a
  `label: value` payload). `HwBridge.exe --shm` publishes its sensors there and `asterctl --shm` reads them,
  replacing the `hwbridge.txt` file hand-off; a sequence number plus timestamp let asterctl detect fresh vs.
  stale data (useful after sleep/resume). The legacy text-file mode remains as fallback (`asterctl` still
  reads `cfg/sensors/*.txt`; `HwBridge` without `--shm` still writes `hwbridge.txt`).
- `aster-launcher` suspends all children while Windows sleeps and respawns them after wake (power-event monitor).
- On wake, the launcher optionally disables + re-enables the AOOSTAR USB UART before respawning children
  (`restart_uart_on_resume`, default `true`) — automates the old manual Device Manager fix.
- Shared `refresh_time` option in `launcher.toml` sets the sensor refresh interval for both
  `aster-sysinfo` and `hwbridge`; allowed values are 2, 5, 10, or 30 seconds. The legacy per-process
  keys `sysinfo_refresh` / `hwbridge_refresh` still work when `refresh_time` is not set, and out-of-range
  values are rejected instead of causing a respawn loop.
- The refresh interval can be changed live from the tray icon (`Refresh time` sub-menu with
  2s / 5s / 10s / 30s): the choice is written back to `launcher.toml` and `aster-sysinfo` + `hwbridge`
  are restarted automatically to apply it; the active value is marked with a check mark.
- Vendored `tray-item` (see `vendor/tray-item`) with Windows sub-menu support added — the crates.io
  release has no sub-menu API and cannot deliver sub-menu item clicks.

### Fixed
- `asterctl` no longer exits when the LCD serial port fails (e.g. after resume); it reopens the port with
  backoff and re-initializes the display in-process.

## v0.2.0 - 2025-08-31
### Fixed
- Misplaced text sensors in custom panels ([#11](https://github.com/zehnm/aoostar-rs/issues/11)).
- Wrong start position for circular progress (fan) sensor using a counter-clockwise direction ([#12](https://github.com/zehnm/aoostar-rs/issues/12)).
- aster-sysinfo tool: make sensor file world-readable, create all parent directories.

### Added
- Simple sensor panel with a file-based data source ([#6](https://github.com/zehnm/aoostar-rs/issues/6)). 
- Initial support for fan-, progress-, & pointer-sensors ([#8](https://github.com/zehnm/aoostar-rs/pull/8)).
- Use [mdBook](https://rust-lang.github.io/mdBook/) for documentation and publish user guide to GitHub pages ([#10](https://github.com/zehnm/aoostar-rs/pull/10)).
- Initial `aster-sysinfo` tool for providing sensor values in a text file for `asterctl`.

### Changed
- Project structure using a Cargo workspace.

---

## v0.1.0 - 2025-08-02
### Added
- Initial `asterctl` tool release for controlling the LCD: on, off, display an image.
- systemd service file to switch off LCD on system start.
- Demo mode.
