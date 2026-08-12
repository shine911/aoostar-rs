# Windows single-exe launcher

Date: 2026-08-12

## Problem

Windows users currently must build and run 3 separate long-running processes:
`aster-sysinfo.exe`, `asterctl.exe`, and `hwbridge\HwBridge.exe` (a vendored .NET
Framework tool requiring Administrator, see `docs/windows/README.md`). Today
`windows/start-services.ps1` opens 3 visible console windows to do this.

Goal: ship one exe end users double-click. It starts all 3 as hidden background
processes, needs no PowerShell, and needs no manual multi-window juggling.

## Architecture

New workspace crate `crates/aster-launcher` → `aster-launcher.exe`.

Windows-only logic is behind `#[cfg(windows)]`. The crate must still compile on
Linux, since CI (`.github/workflows/build.yml`) runs `cargo build --release --bins
--all-features` on `ubuntu-22.04` across the whole workspace. On non-Windows,
`main()` is a stub that prints "aster-launcher is Windows-only" and exits
non-zero — no functional behavior is expected or tested there.

The exe embeds a Windows manifest with
`requestedExecutionLevel=requireAdministrator` (via a `build.rs` using the
`winres`/`embed-manifest` crate). Windows shows exactly one UAC prompt when the
user double-clicks the exe, before any code runs — no runtime re-exec/`runas`
logic needed. All 3 spawned children inherit the elevated token, so hwbridge
(the only one that actually needs Admin) works without a second prompt.

## Components

- `config.rs` — reads `launcher.toml` from the exe's own directory
  (`std::env::current_exe()` parent). Fields: `monitor_config` (default
  `Monitor3.json`), `sysinfo_refresh` (default `2`), `hwbridge_refresh`
  (default `5`). Missing file or unparseable fields fall back to defaults and
  log a warning — never a hard failure.

- `process.rs` — spawns the 3 children with `std::process::Command`, flag
  `CREATE_NO_WINDOW` so nothing is visible. Each child's stdout/stderr is
  piped to `logs/<name>.log` next to the exe (truncated at launcher startup).
  One watcher thread per child: if the child exits unexpectedly (non-zero or
  crash) while the launcher is still running, it logs a `--- restarted at
  <time> ---` marker to that child's log and relaunches it with the same
  arguments. All child paths are resolved relative to the launcher's own exe
  directory, matching the current `start-services.ps1` path convention
  (`bin\aster-sysinfo.exe`, `bin\asterctl.exe`, `hwbridge\HwBridge.exe`).

- `tray.rs` — a `tray-icon`-based tray icon with a right-click menu: current
  status (running / degraded — degraded means at least one child is down and
  awaiting restart) and "Quit All". Quit terminates all 3 children and exits
  the launcher. No other UI; no window is shown.

## Data flow

1. User double-clicks `aster-launcher.exe` in `dist/`.
2. Windows shows one UAC prompt (manifest-driven); user accepts.
3. Launcher reads `launcher.toml` (or defaults).
4. Launcher spawns `aster-sysinfo.exe --out cfg\sensors\sysinfo.txt --temp-dir
   cfg\sensors --refresh <sysinfo_refresh>`, `asterctl.exe --config
   <monitor_config>`, and `hwbridge\HwBridge.exe cfg\sensors\hwbridge.txt
   <hwbridge_refresh>`, each hidden, each logging to `logs/`.
5. Tray icon appears; status reflects child health.
6. If a child crashes, its watcher thread restarts it and logs the event.
7. User right-clicks tray → Quit All → all 3 children killed, launcher exits.

## Packaging

New `windows/package-dist.ps1`, styled like the existing
`windows/start-services.ps1`. Run after `cargo build --release` and after
building `HwBridge.exe` (per current `docs/windows/README.md` steps). It
assembles a self-contained `dist/` folder:

```
dist/
  aster-launcher.exe
  bin/
    aster-sysinfo.exe
    asterctl.exe
  hwbridge/
    HwBridge.exe
    *.dll
  cfg/
    Monitor3.json, monitor.json, *.jpg, *.png, sensor-mapping/, sensor-mapping.cfg
  launcher.toml         (default values, user-editable)
```

`windows/start-services.ps1` is removed — superseded by the launcher exe.
`docs/windows/README.md` is updated: build steps stay the same, but the
"Running" section is replaced with "run `windows/package-dist.ps1`, then
double-click `dist\aster-launcher.exe`."

## Error handling

- Missing child exe/DLL at launcher startup: log an error for that child,
  mark it degraded in the tray, do not crash the launcher or block the other
  2 children from starting.
- Child crash mid-run: auto-restart + log marker (see `process.rs` above).
- Malformed `launcher.toml`: fall back to per-field defaults, log a warning
  naming which field(s) were invalid.

## Testing

- Unit tests for `config.rs`: missing file, empty file, partially-specified
  TOML, malformed TOML — all resolve to sane values, none panic.
- Manual verification on a real Windows machine (CI is Linux-only and cannot
  exercise process spawn / UAC / tray):
  - Double-click shows exactly one UAC prompt.
  - All 3 children run with no visible console windows.
  - `logs/*.log` populate with expected content.
  - Killing one child via Task Manager triggers auto-restart within a few
    seconds, with a restart marker in its log.
  - Tray "Quit All" terminates all 3 processes and the launcher exits.
