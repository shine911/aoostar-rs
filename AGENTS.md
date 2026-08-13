# aoostar-rs

Reverse-engineered open client for the AOOSTAR WTR MAX / GEM12+ PRO second LCD screen. Rust workspace;
on Windows a small C# bridge (`hwbridge`) fills in sensors `aster-sysinfo` cannot read.

## Project

- Stack: Rust (edition 2024, `rust-version = "1.88"`, workspace `members = ["crates/*"]`); Windows uses
  `aster-launcher.exe` (tray icon) to run all children hidden + elevated; C# `.NET Framework` for `HwBridge.exe`.
- Entry points: `crates/asterctl` (LCD control, sensor panels), `crates/aster-sysinfo` (sensor provider → text
  files in `cfg/sensors/`), `crates/aster-launcher` (Windows-only supervisor), `crates/asterctl-lcd` (serial
  protocol library), `hwbridge/HwBridge.cs` (Windows-only hardware sensors).
- Runtime config: `dist/launcher.toml` (built from `windows/launcher.default.toml`). `dist/` is gitignored
  (output of `windows/package-dist.ps1`).

## Commands

- Workspace: `cargo build --release`; tests: `cargo test -p aster-launcher`, `cargo test -p aster-sysinfo`.
- **From WSL (this dev environment):** host has NO C compiler (`cc` missing), no root, and cargo is not on
  PATH. Use `./build-from-wsl.sh [test|build|windows-check|windows-build]` — `test`/`build`/`windows-check`
  run in a Debian docker container (gcc + rustup `1.97`, `CARGO_TARGET_DIR=/tmp/target` so repo `target/`
  stays clean); `windows-build` calls the REAL Windows toolchain directly from WSL via interop
  (`cargo.exe` + `csc.exe` + `powershell.exe`) and produces `dist\` — needs the repo on `/mnt/c` and
  `aster-launcher.exe` not running. For direct cargo use: `export PATH="$HOME/.cargo/bin:$PATH"`.
- Formatting: `cargo fmt --all -- --check` (repo is currently fmt-clean).
- Windows (deployables): build on Windows (`cargo build --release`), then `windows/package-dist.ps1`.
  `cargo test -p aster-launcher` on Windows needs an elevated shell or `__COMPAT_LAYER=RunAsInvoker`
  (the crate links a `requireAdministrator` manifest).
- hwbridge (C#, built on Windows, not from WSL):
  `C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe /nologo /r:LibreHardwareMonitorLib.dll /out:HwBridge.exe HwBridge.cs`
- aster-sysinfo flags: `--out <file> --temp-dir <dir> --refresh <2|5|10|30> [--disk-refresh <secs>] [--smartctl] [--console]`
- asterctl: `asterctl --config Monitor3.json` (configs + images in `cfg/`, sensor files in `cfg/sensors/`).

## Architecture

- `crates/aster-launcher` — Windows supervisor: tray with status + live refresh-interval menu (`tray.rs`),
  power sleep/resume (`power.rs`), child restart-with-backoff watchers reading shared specs (`process.rs`),
  UART re-enumeration on wake (`device.rs`), `launcher.toml` loading (`config.rs`), never-panic logging (`logging.rs`).
- `crates/aster-sysinfo` — `SysinfoSource` (sysinfo crate: cpu/mem/swap/disks/net/temps) + Linux-only
  per-disk storage/`smartctl` sensors. Writes `label: value` text file atomically.
- `crates/asterctl` — LCD display: parses AOOSTAR-X `Monitor*.json` panels, renders, rotates, reads sensor
  values from text files, speaks the serial protocol (`asterctl-lcd`).
- `hwbridge/HwBridge.cs` — loads the same `LibreHardwareMonitorLib.dll` AOOSTAR-X uses, writes
  CPU/GPU/mobo/memory temps + GPU load to `cfg/sensors/hwbridge.txt`; needs Administrator.
- `cfg/` — `Monitor3.json` (default panel config), `sensor-mapping.cfg`, `sensors/*.txt` (runtime output,
  gitignored).

## Conventions

- SPDX headers `MIT OR Apache-2.0` on Rust files; `#![forbid(non_ascii_idents)]` + `#![deny(unsafe_code)]` in binaries.
- aster-launcher: never-panic contract — config/logging never return `Err` (bad TOML → defaults + log line);
  Windows-only runtime; unit tests must stay cross-platform (Windows-only code is manually verified).
- Refresh intervals are constrained to {2, 5, 10, 30}s everywhere (`config.rs` `REFRESH_OPTIONS`, aster-sysinfo
  `--refresh` parser, `HwBridge.cs`). `refresh_time` (launcher.toml) is the shared option; legacy
  `sysinfo_refresh` / `hwbridge_refresh` still honored when it is unset (backward compat).
- `HwBridge.cs` is deliberately old-style C# (no tuples, no string interpolation, no LINQ) — keep it that way
  so it compiles with plain .NET Framework `csc`.
- Changelog: `CHANGELOG.md` (Keep a Changelog); docs in `docs/`; branch of record: `windows`.

## Notes

- TODO/known-issues live in `TODO.md` (e.g. tray edge cases, `launcher.log` rotation, config validation polish).
- WSL build recipe + gotchas are documented in `build-from-wsl.sh` (header).
