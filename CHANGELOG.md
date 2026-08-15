# AOOSTAR WTR MAX Screen Control Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added
- `aster-launcher` tray "Display" sub-menu with three mutually exclusive LCD modes: **On**, **Off**,
  and **Follow screen state** (the active mode carries a check mark). Picking one persists
  `display_mode = "on" | "off" | "follow"` in `launcher.toml` and applies it immediately.
  Manual On/Off need no process restart: the launcher writes `cfg/display.state`, which
  `asterctl --display-state` (new flag) polls on every refresh — `off` turns the LCD off and skips
  rendering while the serial port stays open, so picking **On** wakes it instantly.
- "Follow screen state" mirrors the Windows console display power state (off/on/dimmed, via
  `PowerSettingRegisterNotification` with `GUID_CONSOLE_DISPLAY_STATE`) into the same state file,
  so the LCD turns off when the monitor turns off (power button, idle timeout, screensaver, lid
  close) and back on when it turns on — including idle-blank transitions that never enter system sleep.
- The LCD turns off automatically when `aster-launcher` is closed or killed (e.g. via Task Manager):
  the launcher rewrites `cfg/display.state` roughly every 2s as a heartbeat, and if `asterctl` sees
  the file go stale (~10s) it switches the display off and exits to free the serial port. Quitting
  the launcher from the tray blanks the display the same way before the child processes are stopped.

## v0.3.0 - 2026-08-14

### Added
- New `aster-hwstats` crate: shared-memory hardware stats protocol (Windows named file mapping
  `AOOSTAR_HW_STATS`, two producer slots with per-slot `magic`/`version`/`sequence`/`timestamp` header and a
  `label: value` payload). `HwBridge.exe --shm` and `aster-sysinfo --shm` publish their sensors there and
  `asterctl --shm` reads both slots, replacing the `hwbridge.txt` / `sysinfo.txt` file hand-off on Windows;
  sequence numbers plus timestamps let asterctl detect fresh vs. stale data (useful after sleep/resume).
  The legacy text-file mode remains as fallback (`aster-sysinfo --out`, `HwBridge` without `--shm`, and
  `asterctl` without `--shm` still use `cfg/sensors/*.txt`).
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
- `aster-launcher` tray "Themes" sub-menu with the official AOOSTAR-X theme options (Default, Cyberpunk,
  Interstellar, Cartoon): picking one persists `theme` in `launcher.toml` and restarts `asterctl`, which
  activates the matching built-in panel pair. `asterctl --theme <0-3>` selects the theme from the command
  line (the CLI flag wins over `setup.theme` in the monitor config; without either, the config's own
  `mianban` is used unchanged). Theme handling mirrors the official AOOSTAR-X behavior: theme N activates
  panels `(1+2N, 2+2N)` filtered by the `controlParams` / `controlDiskTemp` flags.
- GitHub Actions: the CI workflow additionally type-checks the Windows-only launcher code (`cargo check`
  for the `x86_64-pc-windows-msvc` target), and a new release workflow builds the full Windows `dist\`
  package (release binaries + `HwBridge.exe` + cfg/fonts, via `windows/package-dist.ps1`) and uploads
  `aoostar-rs-<tag>-windows-x64.zip` (with SHA-256 checksum) to the GitHub release.

### Fixed
- `asterctl` no longer exits when the LCD serial port fails (e.g. after resume); it reopens the port with
  backoff and re-initializes the display in-process.
- LCD frames are now flushed to the serial port once per frame instead of once per 59-byte chunk, removing
  ~15,380 blocking `tcdrain` calls per full frame and letting the kernel pipeline chunks at full UART
  throughput (cherry-picked from upstream [#26](https://github.com/zehnm/aoostar-rs/pull/26)).
- `asterctl` now retries the startup display init with backoff (1s → 2s → … → 32s, ~6 attempts) before
  giving up, so a screen that is still waking up after USB re-enumeration comes back in-process instead of
  crash-looping on the first write timeout.
- `aster-launcher` now backs off when a child repeatedly exits with a failure shortly after spawn (e.g.
  `asterctl` failing the display init after resume) instead of restarting it every few seconds forever,
  hammering the serial port.
- `aster-launcher` now re-enumerates the AOOSTAR USB UART on wake with the function-level reset
  `CM_Reset_Device` (an in-place USB port reset) instead of Device Manager-style disable/enable, which
  could leave the device in the "restart required" pending state; disable/enable is only used as a
  fallback where `CM_Reset_Device` is not exported (documented as Windows 10 1809+, but verified
  absent from `CfgMgr32.dll` even on Windows 10 25H2 build 26200, so the fallback is what runs on the
  WTR MAX).
- `aster-launcher` now waits for the UART restart to finish before respawning the children after wake.
  Previously the `suspended` flag was cleared immediately on resume, so `asterctl` could reopen COM3
  while the UART was being disabled, making `CM_Disable_DevNode` fail (`CR_INSUFFICIENT_RESOURCES`,
  launcher.log "USB UART restart failed: DisableFailed(23)") and leaving the port wedged — the LCD then
  never recovered and `asterctl.log` filled with "semaphore timeout (os error 121)" init retries.

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
