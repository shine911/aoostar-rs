# Windows Single Launcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new `aster-launcher.exe` that Windows users double-click to start `aster-sysinfo`, `asterctl`, and `hwbridge\HwBridge.exe` as hidden background processes (one UAC prompt, tray icon, auto-restart on crash), replacing the current `windows/start-services.ps1` 3-window workflow.

**Architecture:** New workspace crate `crates/aster-launcher`. Windows-only runtime logic (process spawn with hidden window, tray icon, Admin elevation manifest) lives behind `#[cfg(windows)]`; config parsing and path-resolution logic stay platform-neutral so they compile and unit-test on the project's Linux CI. A new `windows/package-dist.ps1` assembles a self-contained `dist/` folder; `windows/start-services.ps1` is removed.

**Tech Stack:** Rust (edition 2024), `serde` + `toml` for config, `tray-item` (Windows-target-only dependency) for the tray icon, `winresource` (build-dependency) to embed a `requireAdministrator` manifest, `chrono` for log timestamps — all versions matching or resolved the same way as the rest of the workspace.

## Global Constraints

- Rust version floor `1.88`, edition `2024` (workspace package defaults in root `Cargo.toml` — new crate inherits via `.workspace = true`).
- New crate must pass on the existing Linux CI job (`.github/workflows/build.yml`): `cargo fmt --all -- --check`, clippy clean, `cargo test`, and `cargo build --release --bins --all-features` — all on `ubuntu-22.04`/`ubuntu-24.04`. Any code using Windows-only APIs or crates must be behind `#[cfg(windows)]` (module- or function-level) and any Windows-only *dependency* must be declared under `[target.'cfg(windows)'.dependencies]` in `Cargo.toml`, not as a plain dependency — otherwise Linux CI tries to compile it (e.g. `tray-item`'s Linux backend needs GTK, which CI does not install) and the build breaks.
- Match existing crate conventions: `#![forbid(non_ascii_idents)]`, `#![deny(unsafe_code)]` at the top of `main.rs` (see `crates/asterctl/src/main.rs`, `crates/aster-sysinfo/src/main.rs`).
- No Rust toolchain is available in the authoring sandbox used to write this plan. Every build/test step below must be run on a machine with `cargo` installed. Anything inside `#[cfg(windows)]` can only be exercised on Windows; run those verification steps there (the project's normal build target — see `docs/windows/README.md`).
- `HwBridge.exe` is invoked as `HwBridge.exe <out-file> <refresh-seconds>` (positional args, confirmed at `hwbridge/HwBridge.cs:70,74`). `aster-sysinfo` and `asterctl` args match the ones already used in `windows/start-services.ps1`.

---

## File Structure

```
crates/aster-launcher/
  Cargo.toml
  build.rs                     # embeds requireAdministrator manifest (Windows target only)
  aster-launcher.manifest      # the manifest XML build.rs embeds
  src/
    main.rs                    # entry point, cfg(windows) dispatch, windows_main()
    config.rs                  # LauncherConfig + load() — cross-platform, unit tested
    logging.rs                 # append_line() timestamped file logger — cross-platform, unit tested
    process.rs                 # ChildSpec/child_specs() cross-platform + unit tested;
                                # spawn_and_watch()/spawn_child() cfg(windows)-only
    tray.rs                    # cfg(windows)-only: tray icon + Quit menu + status loop
windows/
  package-dist.ps1             # new: assembles dist/ from release build + hwbridge/ + cfg/
  launcher.default.toml        # new: default launcher.toml copied into dist/
  start-services.ps1           # deleted — superseded by aster-launcher.exe
docs/windows/README.md         # "Running" section rewritten for package-dist.ps1 + the launcher
```

---

### Task 1: Scaffold the `aster-launcher` crate

**Files:**
- Create: `crates/aster-launcher/Cargo.toml`
- Create: `crates/aster-launcher/src/main.rs`

**Interfaces:**
- Produces: crate `aster-launcher`, binary target `aster-launcher`, buildable on any host via its non-Windows stub `main()`.

- [ ] **Step 1: Create the crate manifest**

```toml
# crates/aster-launcher/Cargo.toml
[package]
name = "aster-launcher"
version = "0.1.0"
description = "Single-exe launcher for aster-sysinfo, asterctl, and hwbridge"

rust-version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
serde = { version = "1.0.219", features = ["derive"] }
chrono = "0.4"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the cross-platform stub entry point**

```rust
// crates/aster-launcher/src/main.rs
#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]

mod config;
mod logging;
mod process;

fn main() {
    #[cfg(windows)]
    windows_main();

    #[cfg(not(windows))]
    {
        eprintln!("aster-launcher is Windows-only.");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn windows_main() {
    unimplemented!("wired up in a later task")
}
```

Create empty placeholder files so this compiles:

```rust
// crates/aster-launcher/src/config.rs
```

```rust
// crates/aster-launcher/src/logging.rs
```

```rust
// crates/aster-launcher/src/process.rs
```

- [ ] **Step 3: Verify the workspace picks up the new crate and it builds**

Run: `cargo build -p aster-launcher`
Expected: builds successfully. On a non-Windows host this compiles the `#[cfg(not(windows))]` branch; on Windows it will fail at runtime on the `unimplemented!()` if run, which is expected until Task 8.

- [ ] **Step 4: Commit**

```bash
git add crates/aster-launcher
git commit -m "feat(aster-launcher): scaffold crate with cross-platform stub"
```

---

### Task 2: `config.rs` — `LauncherConfig`

**Files:**
- Modify: `crates/aster-launcher/src/config.rs`
- Modify: `crates/aster-launcher/Cargo.toml`

**Interfaces:**
- Produces: `pub struct LauncherConfig { pub monitor_config: String, pub sysinfo_refresh: u16, pub hwbridge_refresh: u16 }` implementing `Default`, and `pub fn LauncherConfig::load(path: &std::path::Path) -> LauncherConfig`. Later tasks (`process.rs::child_specs`, `main.rs::windows_main`) consume this type and its 3 fields by name.

- [ ] **Step 1: Add the `toml` dependency**

Run: `cd crates/aster-launcher && cargo add toml`
Expected: `Cargo.toml` gains a `toml = "<resolved version>"` line under `[dependencies]`.

- [ ] **Step 2: Write the failing tests**

```rust
// crates/aster-launcher/src/config.rs
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub monitor_config: String,
    pub sysinfo_refresh: u16,
    pub hwbridge_refresh: u16,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            monitor_config: "Monitor3.json".to_string(),
            sysinfo_refresh: 2,
            hwbridge_refresh: 5,
        }
    }
}

impl LauncherConfig {
    /// Loads from `path`. Falls back to [`LauncherConfig::default`] (as a
    /// whole, or per-field for a partially-specified file) if the file is
    /// missing or cannot be parsed. Never panics, never returns `Err` —
    /// there is no valid state for the launcher to refuse to start in.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                crate::logging::append_line(
                    &path.with_file_name("launcher.log"),
                    &format!("launcher.toml is invalid, using defaults: {err}"),
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    #[test]
    fn missing_file_returns_defaults() {
        let path = Path::new("this-file-does-not-exist.toml");
        assert_eq!(LauncherConfig::load(path), LauncherConfig::default());
    }

    #[test]
    fn empty_file_returns_defaults() {
        let file = write_temp("");
        assert_eq!(LauncherConfig::load(file.path()), LauncherConfig::default());
    }

    #[test]
    fn partial_file_fills_missing_fields_with_defaults() {
        let file = write_temp("sysinfo_refresh = 9\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.sysinfo_refresh, 9);
        assert_eq!(cfg.monitor_config, LauncherConfig::default().monitor_config);
        assert_eq!(cfg.hwbridge_refresh, LauncherConfig::default().hwbridge_refresh);
    }

    #[test]
    fn malformed_file_returns_defaults() {
        let file = write_temp("sysinfo_refresh = \"not a number\"\n");
        assert_eq!(LauncherConfig::load(file.path()), LauncherConfig::default());
    }
}
```

- [ ] **Step 3: Run the tests to see them compile and pass (or fail on a real bug, not a missing type)**

Run: `cargo test -p aster-launcher config::`
Expected: 4 tests pass (`missing_file_returns_defaults`, `empty_file_returns_defaults`, `partial_file_fills_missing_fields_with_defaults`, `malformed_file_returns_defaults`).

If `partial_file_fills_missing_fields_with_defaults` fails because `toml`/`serde`'s container-level `#[serde(default)]` didn't fill the omitted fields, add `#[serde(default)]` to each individual field as well (this is the more portable form across `toml` crate versions):

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LauncherConfig {
    #[serde(default = "LauncherConfig::default_monitor_config")]
    pub monitor_config: String,
    #[serde(default = "LauncherConfig::default_sysinfo_refresh")]
    pub sysinfo_refresh: u16,
    #[serde(default = "LauncherConfig::default_hwbridge_refresh")]
    pub hwbridge_refresh: u16,
}

impl LauncherConfig {
    fn default_monitor_config() -> String {
        "Monitor3.json".to_string()
    }
    fn default_sysinfo_refresh() -> u16 {
        2
    }
    fn default_hwbridge_refresh() -> u16 {
        5
    }
}
```

(keep the `impl Default for LauncherConfig` using the same 3 values so `LauncherConfig::default()` and a fully-empty-file parse agree.)

- [ ] **Step 4: Commit**

```bash
git add crates/aster-launcher/Cargo.toml crates/aster-launcher/Cargo.lock crates/aster-launcher/src/config.rs
git commit -m "feat(aster-launcher): add LauncherConfig with defaulting toml loader"
```

---

### Task 3: `logging.rs` — timestamped file logging

**Files:**
- Modify: `crates/aster-launcher/src/logging.rs`

**Interfaces:**
- Consumes: nothing from other modules.
- Produces: `pub fn append_line(path: &std::path::Path, message: &str)`. Used by `config.rs` (Task 2, already written above) and by `process.rs` (Task 5) for restart markers.

- [ ] **Step 1: Write the failing test**

```rust
// crates/aster-launcher/src/logging.rs
use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Appends one timestamped line to `path`, creating the file and any
/// missing parent directories if needed. Never panics: a logging failure
/// must not take down the launcher.
pub fn append_line(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "[{}] {}", Local::now().format("%Y-%m-%d %H:%M:%S"), message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lines_with_timestamp_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("test.log");

        append_line(&path, "first");
        append_line(&path, "second");

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("first"));
        assert!(lines[1].ends_with("second"));
        assert!(lines[0].starts_with('['));
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p aster-launcher logging::`
Expected: `appends_lines_with_timestamp_prefix` passes.

- [ ] **Step 3: Commit**

```bash
git add crates/aster-launcher/src/logging.rs
git commit -m "feat(aster-launcher): add timestamped append-only file logger"
```

---

### Task 4: `process.rs` — `ChildSpec` and `child_specs` (cross-platform, pure)

**Files:**
- Modify: `crates/aster-launcher/src/process.rs`

**Interfaces:**
- Consumes: `config::LauncherConfig` (Task 2) fields `monitor_config: String`, `sysinfo_refresh: u16`, `hwbridge_refresh: u16`.
- Produces: `pub struct ChildSpec { pub name: &'static str, pub base_dir: std::path::PathBuf, pub exe_path: std::path::PathBuf, pub args: Vec<String>, pub log_path: std::path::PathBuf }` and `pub fn child_specs(base_dir: &std::path::Path, cfg: &LauncherConfig) -> [ChildSpec; 3]`. Task 5's `spawn_child` and Task 8's `windows_main` consume both by these exact names.

- [ ] **Step 1: Write the failing test**

```rust
// crates/aster-launcher/src/process.rs
use crate::config::LauncherConfig;
use std::path::{Path, PathBuf};

pub struct ChildSpec {
    pub name: &'static str,
    pub base_dir: PathBuf,
    pub exe_path: PathBuf,
    pub args: Vec<String>,
    pub log_path: PathBuf,
}

/// Builds the 3 child process specs, all paths relative to `base_dir` (the
/// launcher's own exe directory) so a copied/zipped `dist/` folder keeps
/// working wherever it's placed.
pub fn child_specs(base_dir: &Path, cfg: &LauncherConfig) -> [ChildSpec; 3] {
    let logs_dir = base_dir.join("logs");
    [
        ChildSpec {
            name: "aster-sysinfo",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("bin").join("aster-sysinfo.exe"),
            args: vec![
                "--out".to_string(),
                "cfg\\sensors\\sysinfo.txt".to_string(),
                "--temp-dir".to_string(),
                "cfg\\sensors".to_string(),
                "--refresh".to_string(),
                cfg.sysinfo_refresh.to_string(),
            ],
            log_path: logs_dir.join("aster-sysinfo.log"),
        },
        ChildSpec {
            name: "asterctl",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("bin").join("asterctl.exe"),
            args: vec!["--config".to_string(), cfg.monitor_config.clone()],
            log_path: logs_dir.join("asterctl.log"),
        },
        ChildSpec {
            name: "hwbridge",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("hwbridge").join("HwBridge.exe"),
            args: vec![
                "cfg\\sensors\\hwbridge.txt".to_string(),
                cfg.hwbridge_refresh.to_string(),
            ],
            log_path: logs_dir.join("hwbridge.log"),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_specs_relative_to_base_dir_using_config_values() {
        let base_dir = Path::new("C:\\dist");
        let cfg = LauncherConfig {
            monitor_config: "Custom.json".to_string(),
            sysinfo_refresh: 7,
            hwbridge_refresh: 11,
        };

        let specs = child_specs(base_dir, &cfg);

        assert_eq!(specs[0].name, "aster-sysinfo");
        assert_eq!(specs[0].exe_path, base_dir.join("bin").join("aster-sysinfo.exe"));
        assert_eq!(specs[0].args.last().unwrap(), "7");
        assert_eq!(specs[0].log_path, base_dir.join("logs").join("aster-sysinfo.log"));

        assert_eq!(specs[1].name, "asterctl");
        assert_eq!(specs[1].exe_path, base_dir.join("bin").join("asterctl.exe"));
        assert_eq!(specs[1].args, vec!["--config".to_string(), "Custom.json".to_string()]);

        assert_eq!(specs[2].name, "hwbridge");
        assert_eq!(specs[2].exe_path, base_dir.join("hwbridge").join("HwBridge.exe"));
        assert_eq!(
            specs[2].args,
            vec!["cfg\\sensors\\hwbridge.txt".to_string(), "11".to_string()]
        );

        for spec in &specs {
            assert_eq!(spec.base_dir, base_dir);
        }
    }
}
```

Note: `LauncherConfig` needs `Clone`/public fields to be constructed like this in the test — it already derives `Clone` and has public fields from Task 2.

- [ ] **Step 2: Run the test**

Run: `cargo test -p aster-launcher process::`
Expected: `builds_specs_relative_to_base_dir_using_config_values` passes.

- [ ] **Step 3: Commit**

```bash
git add crates/aster-launcher/src/process.rs
git commit -m "feat(aster-launcher): add cross-platform ChildSpec/child_specs"
```

---

### Task 5: `process.rs` — spawn, hide, and auto-restart children (Windows-only)

**Files:**
- Modify: `crates/aster-launcher/src/process.rs`

**Interfaces:**
- Consumes: `ChildSpec` (Task 4), `logging::append_line` (Task 3).
- Produces (all `#[cfg(windows)]`): `pub struct ChildHandle { pub name: &'static str, pub healthy: std::sync::Arc<std::sync::atomic::AtomicBool>, pub current_child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>> }` and `pub fn spawn_and_watch(spec: ChildSpec, quit: std::sync::Arc<std::sync::atomic::AtomicBool>) -> ChildHandle`. Task 7 (`tray.rs`) reads `ChildHandle::healthy`; Task 8 (`main.rs`) calls `spawn_and_watch` and, on quit, locks `current_child` to kill it.

This task has no automated test — it spawns real OS processes, which the spec's Testing section explicitly defers to manual verification on a real Windows machine (CI is Linux-only and cannot exercise process spawn). Implement it directly, then manually verify per the steps below.

- [ ] **Step 1: Implement spawn/hide/restart, appended to `process.rs`**

```rust
#[cfg(windows)]
pub struct ChildHandle {
    pub name: &'static str,
    pub healthy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub current_child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn spawn_child(spec: &ChildSpec) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)?;
    let log_file_err = log_file.try_clone()?;

    std::process::Command::new(&spec.exe_path)
        .args(&spec.args)
        .current_dir(&spec.base_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(log_file)
        .stderr(log_file_err)
        .spawn()
}

/// Spawns `spec` in a background thread that keeps it running: on
/// unexpected exit it logs a restart marker and relaunches, until `quit`
/// is set. Returns immediately with a handle to observe health / reach the
/// current child for a forced kill.
#[cfg(windows)]
pub fn spawn_and_watch(
    spec: ChildSpec,
    quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> ChildHandle {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let healthy = Arc::new(AtomicBool::new(false));
    let current_child: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));

    let handle = ChildHandle {
        name: spec.name,
        healthy: healthy.clone(),
        current_child: current_child.clone(),
    };

    std::thread::spawn(move || {
        loop {
            if quit.load(Ordering::SeqCst) {
                break;
            }

            match spawn_child(&spec) {
                Ok(child) => {
                    healthy.store(true, Ordering::SeqCst);
                    *current_child.lock().unwrap() = Some(child);

                    // Block until the child exits, without holding the lock
                    // (so a quit-triggered kill() from another thread can
                    // still reach it).
                    let status = loop {
                        let mut guard = current_child.lock().unwrap();
                        match guard.as_mut() {
                            Some(child) => match child.try_wait() {
                                Ok(Some(status)) => break Some(status),
                                Ok(None) => {}
                                Err(_) => break None,
                            },
                            None => break None,
                        }
                        drop(guard);
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    };

                    *current_child.lock().unwrap() = None;
                    healthy.store(false, Ordering::SeqCst);

                    if quit.load(Ordering::SeqCst) {
                        break;
                    }

                    crate::logging::append_line(
                        &spec.log_path,
                        &format!("--- {} exited ({status:?}), restarting ---", spec.name),
                    );
                }
                Err(err) => {
                    healthy.store(false, Ordering::SeqCst);
                    crate::logging::append_line(
                        &spec.log_path,
                        &format!("failed to start {}: {err}", spec.name),
                    );
                }
            }

            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });

    handle
}
```

- [ ] **Step 2: Verify it compiles on Windows**

Run (on a Windows machine with `cargo`): `cargo build -p aster-launcher --target x86_64-pc-windows-msvc` (or plain `cargo build -p aster-launcher` if already on Windows).
Expected: builds with no errors. On non-Windows this whole block is compiled out, so `cargo build -p aster-launcher` there is unaffected (still just the Task 1 stub + Tasks 2-4 cross-platform code).

- [ ] **Step 3: Commit**

```bash
git add crates/aster-launcher/src/process.rs
git commit -m "feat(aster-launcher): spawn children hidden with auto-restart"
```

---

### Task 6: Admin elevation manifest

**Files:**
- Create: `crates/aster-launcher/build.rs`
- Create: `crates/aster-launcher/aster-launcher.manifest`
- Modify: `crates/aster-launcher/Cargo.toml`

**Interfaces:**
- Produces: a build-time step that embeds `requestedExecutionLevel=requireAdministrator` into `aster-launcher.exe`, so Windows shows one UAC prompt on launch and every spawned child (Task 5) inherits the elevated token.

- [ ] **Step 1: Add the build-dependency**

Run: `cd crates/aster-launcher && cargo add --build winresource`
Expected: `Cargo.toml` gains a `[build-dependencies]` section with `winresource = "<resolved version>"`.

- [ ] **Step 2: Write the manifest**

```xml
<!-- crates/aster-launcher/aster-launcher.manifest -->
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
```

- [ ] **Step 3: Write `build.rs`, guarded so it's a no-op off Windows**

```rust
// crates/aster-launcher/build.rs
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("aster-launcher.manifest");
        res.compile().expect("failed to embed Windows manifest into aster-launcher.exe");
    }
}
```

- [ ] **Step 4: Verify it doesn't break the Linux build**

Run: `cargo build -p aster-launcher`
Expected: builds successfully; `build.rs` runs but skips `winresource::WindowsResource` entirely since `CARGO_CFG_TARGET_OS` is `linux`.

- [ ] **Step 5: Verify the manifest embeds on Windows**

Run (on Windows): `cargo build --release -p aster-launcher`, then right-click the produced `target\release\aster-launcher.exe` → Properties → Compatibility tab, or just double-click it and confirm a UAC prompt appears.
Expected: UAC prompt appears on launch.

- [ ] **Step 6: Commit**

```bash
git add crates/aster-launcher/build.rs crates/aster-launcher/aster-launcher.manifest crates/aster-launcher/Cargo.toml crates/aster-launcher/Cargo.lock
git commit -m "feat(aster-launcher): embed requireAdministrator manifest"
```

---

### Task 7: `tray.rs` — tray icon, status, Quit All (Windows-only)

**Files:**
- Modify: `crates/aster-launcher/src/main.rs` (add `#[cfg(windows)] mod tray;`)
- Create: `crates/aster-launcher/src/tray.rs`
- Modify: `crates/aster-launcher/Cargo.toml`

**Interfaces:**
- Consumes: `process::ChildHandle` (Task 5) — reads `.healthy`, `.name`.
- Produces: `#[cfg(windows)] pub fn run(handles: &[process::ChildHandle], quit: std::sync::Arc<std::sync::atomic::AtomicBool>)`. Blocks until `quit` becomes `true` (either because the Quit menu item was clicked, which this function sets itself, or because the caller set it for another reason). Task 8's `windows_main` calls this and, once it returns, proceeds to kill remaining children.

No automated test — same reasoning as Task 5 (spec defers tray behavior to manual Windows verification).

- [ ] **Step 1: Add the tray-item dependency, Windows-target only**

Run: `cd crates/aster-launcher && cargo add --target 'cfg(windows)' tray-item`
Expected: `Cargo.toml` gains:

```toml
[target.'cfg(windows)'.dependencies]
tray-item = "<resolved version>"
```

This must land under a `[target.'cfg(windows)'.dependencies]` table, not plain `[dependencies]` — confirm by opening `Cargo.toml` after running the command. If `cargo add` put it under `[dependencies]` instead, move it manually.

- [ ] **Step 2: Implement the tray**

```rust
// crates/aster-launcher/src/tray.rs
use crate::process::ChildHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tray_item::TrayItem;

fn status_label(handles: &[ChildHandle]) -> String {
    let down: Vec<&str> = handles
        .iter()
        .filter(|h| !h.healthy.load(Ordering::SeqCst))
        .map(|h| h.name)
        .collect();

    if down.is_empty() {
        "Aster Launcher: all running".to_string()
    } else {
        format!("Aster Launcher: degraded ({})", down.join(", "))
    }
}

fn build_tray(label: &str, quit: Arc<AtomicBool>) -> TrayItem {
    let mut tray = TrayItem::new(label, tray_item::IconSource::Resource("aster-launcher"))
        .expect("failed to create tray icon");
    tray.add_label(label).expect("failed to add tray status label");
    tray.add_menu_item("Quit All", move || {
        quit.store(true, Ordering::SeqCst);
    })
    .expect("failed to add tray Quit All item");
    tray
}

/// Shows the tray icon and blocks, refreshing the status label every 2
/// seconds, until `quit` is set (by the Quit All item, or by the caller).
#[cfg(windows)]
pub fn run(handles: &[ChildHandle], quit: Arc<AtomicBool>) {
    let mut last_label = status_label(handles);
    let mut _tray = build_tray(&last_label, quit.clone());

    while !quit.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(2));
        let label = status_label(handles);
        if label != last_label {
            // tray-item has no in-place label update; rebuild the tray icon
            // with the new label instead.
            last_label = label;
            _tray = build_tray(&last_label, quit.clone());
        }
    }
}
```

- [ ] **Step 3: Wire the module in**

```rust
// crates/aster-launcher/src/main.rs — add near the other `mod` lines
#[cfg(windows)]
mod tray;
```

- [ ] **Step 4: Verify it compiles on Windows**

Run (on Windows): `cargo build -p aster-launcher`
Expected: builds with no errors. If `tray_item::IconSource::Resource("aster-launcher")` fails to resolve (no such embedded icon resource), switch to `tray_item::IconSource::Data { .. }` with a small embedded default icon, or drop the icon argument to whatever no-custom-icon variant the installed `tray-item` version exposes — check its docs on `crates.io`/`docs.rs` for the exact API of the version `cargo add` resolved, since this crate's API has changed across versions.

- [ ] **Step 5: Commit**

```bash
git add crates/aster-launcher/Cargo.toml crates/aster-launcher/Cargo.lock crates/aster-launcher/src/main.rs crates/aster-launcher/src/tray.rs
git commit -m "feat(aster-launcher): add tray icon with status and Quit All"
```

---

### Task 8: `main.rs` — wire it all together

**Files:**
- Modify: `crates/aster-launcher/src/main.rs`

**Interfaces:**
- Consumes: `config::LauncherConfig::load` (Task 2), `process::child_specs`/`spawn_and_watch`/`ChildHandle` (Tasks 4-5), `tray::run` (Task 7).
- Produces: a working `aster-launcher.exe`.

- [ ] **Step 1: Replace the `windows_main` stub**

```rust
// crates/aster-launcher/src/main.rs — full file
#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod logging;
mod process;
#[cfg(windows)]
mod tray;

fn main() {
    #[cfg(windows)]
    windows_main();

    #[cfg(not(windows))]
    {
        eprintln!("aster-launcher is Windows-only.");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn windows_main() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let base_dir = std::env::current_exe()
        .expect("cannot resolve current exe path")
        .parent()
        .expect("exe has no parent directory")
        .to_path_buf();

    std::fs::create_dir_all(base_dir.join("logs")).ok();

    let cfg = config::LauncherConfig::load(&base_dir.join("launcher.toml"));
    let specs = process::child_specs(&base_dir, &cfg);

    let quit = Arc::new(AtomicBool::new(false));
    let handles: Vec<process::ChildHandle> = specs
        .into_iter()
        .map(|spec| process::spawn_and_watch(spec, quit.clone()))
        .collect();

    tray::run(&handles, quit.clone());

    // tray::run returned because quit was set (Quit All clicked) — make
    // sure every watcher thread stops trying to restart, then force-kill
    // whichever child is currently running.
    quit.store(true, Ordering::SeqCst);
    for handle in &handles {
        if let Ok(mut guard) = handle.current_child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}
```

- [ ] **Step 2: Verify the cross-platform stub path still builds**

Run: `cargo build -p aster-launcher`
Expected: builds (compiles the `#[cfg(not(windows))]` branch on a non-Windows host).

- [ ] **Step 3: Manual end-to-end verification on Windows**

Prerequisite: `cargo build --release` at the repo root (builds `aster-sysinfo.exe`, `asterctl.exe`, `aster-launcher.exe`), `HwBridge.exe` built per `docs/windows/README.md`, and a `dist/`-shaped layout in place (this is exactly what Task 9's `package-dist.ps1` produces — for a quick manual check before Task 9 exists, hand-copy `target\release\aster-launcher.exe`, `target\release\aster-sysinfo.exe` → `bin\`, `target\release\asterctl.exe` → `bin\`, `hwbridge\` → `hwbridge\`, `cfg\` → `cfg\`, all next to the launcher exe).

- Double-click `aster-launcher.exe`.
- Expected: exactly one UAC prompt; after accepting, a tray icon appears; no console windows appear; `logs\aster-sysinfo.log`, `logs\asterctl.log`, `logs\hwbridge.log` exist and grow.
- Open Task Manager, end one child process (e.g. `asterctl.exe`).
- Expected: within a few seconds a new `asterctl.exe` process appears, and its log gains a `--- asterctl exited (...), restarting ---` line.
- Right-click the tray icon → "Quit All".
- Expected: all 3 child processes end, `aster-launcher.exe` exits.

- [ ] **Step 4: Commit**

```bash
git add crates/aster-launcher/src/main.rs
git commit -m "feat(aster-launcher): wire config, process spawning, and tray together"
```

---

### Task 9: Packaging script and docs

**Files:**
- Create: `windows/package-dist.ps1`
- Create: `windows/launcher.default.toml`
- Delete: `windows/start-services.ps1`
- Modify: `docs/windows/README.md`

**Interfaces:**
- Consumes: `target/release/aster-launcher.exe`, `target/release/aster-sysinfo.exe`, `target/release/asterctl.exe` (built by `cargo build --release` at repo root), `hwbridge/HwBridge.exe` + its vendored DLLs.
- Produces: a `dist/` folder matching the layout `process::child_specs` (Task 4) expects: `dist\aster-launcher.exe`, `dist\bin\aster-sysinfo.exe`, `dist\bin\asterctl.exe`, `dist\hwbridge\HwBridge.exe` (+ DLLs), `dist\cfg\...`, `dist\launcher.toml`.

- [ ] **Step 1: Write the default config template**

```toml
# windows/launcher.default.toml
# aster-launcher configuration. Edit these values, then restart
# aster-launcher.exe to apply them.

# AOOSTAR-X panel config file passed to asterctl's --config option.
monitor_config = "Monitor3.json"

# aster-sysinfo refresh interval, in seconds.
sysinfo_refresh = 2

# hwbridge refresh interval, in seconds.
hwbridge_refresh = 5
```

- [ ] **Step 2: Write the packaging script**

```powershell
<#
.SYNOPSIS
    Assembles a self-contained dist\ folder around aster-launcher.exe.

.DESCRIPTION
    Run after `cargo build --release` (builds aster-launcher, aster-sysinfo,
    asterctl) and after building hwbridge\HwBridge.exe (see
    docs/windows/README.md). Copies everything aster-launcher needs at
    runtime into dist\, so that folder can be run in place or zipped up and
    handed to another machine.

.EXAMPLE
    .\windows\package-dist.ps1
#>
$ErrorActionPreference = "Stop"

# Repo root is the parent of this script's directory (windows\package-dist.ps1 -> repo root)
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$ReleaseDir = Join-Path $RepoRoot "target\release"
$Dist = Join-Path $RepoRoot "dist"

$RequiredFiles = @(
    (Join-Path $ReleaseDir "aster-launcher.exe"),
    (Join-Path $ReleaseDir "aster-sysinfo.exe"),
    (Join-Path $ReleaseDir "asterctl.exe"),
    (Join-Path $RepoRoot "hwbridge\HwBridge.exe")
)
foreach ($file in $RequiredFiles) {
    if (-not (Test-Path $file)) {
        throw "Missing $file - build it first (see docs/windows/README.md)."
    }
}

if (Test-Path $Dist) {
    Remove-Item -Recurse -Force $Dist
}
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $Dist "bin") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $Dist "hwbridge") | Out-Null

Write-Host "Copying binaries..."
Copy-Item (Join-Path $ReleaseDir "aster-launcher.exe") $Dist
Copy-Item (Join-Path $ReleaseDir "aster-sysinfo.exe") (Join-Path $Dist "bin")
Copy-Item (Join-Path $ReleaseDir "asterctl.exe") (Join-Path $Dist "bin")

Write-Host "Copying hwbridge..."
Copy-Item (Join-Path $RepoRoot "hwbridge\*.exe") (Join-Path $Dist "hwbridge")
Copy-Item (Join-Path $RepoRoot "hwbridge\*.dll") (Join-Path $Dist "hwbridge")

Write-Host "Copying cfg..."
Copy-Item (Join-Path $RepoRoot "cfg") (Join-Path $Dist "cfg") -Recurse

Write-Host "Copying default launcher.toml..."
Copy-Item (Join-Path $RepoRoot "windows\launcher.default.toml") (Join-Path $Dist "launcher.toml")

Write-Host "dist\ ready. Double-click dist\aster-launcher.exe to run."
```

- [ ] **Step 3: Remove the superseded script**

```bash
git rm windows/start-services.ps1
```

- [ ] **Step 4: Update the docs**

In `docs/windows/README.md`, replace the entire `## Running` section (currently the block starting at `## Running` through the end of the file) with:

```markdown
## Packaging and running

After building `asterctl`, `aster-sysinfo`, and `aster-launcher` with `cargo build --release`,
and `HwBridge.exe` as described above, assemble a self-contained folder:

```powershell
.\windows\package-dist.ps1
```

This creates `dist\` containing `aster-launcher.exe` and everything it needs: the other 2 Rust
binaries (in `dist\bin\`), `hwbridge\`, `cfg\`, and a default `launcher.toml`. The `dist\` folder
can be run in place or copied/zipped to another machine.

Double-click `dist\aster-launcher.exe` to start `aster-sysinfo`, `asterctl`, and `hwbridge` as
hidden background processes — no console windows, no manually starting 3 separate tools.
Windows will show a single Administrator prompt (hwbridge needs it to read hardware sensors,
and the other 2 inherit the same elevated process so nothing else needs its own prompt). A tray
icon appears once running; right-click it to see status (running / degraded) or choose
"Quit All" to stop everything.

Edit `dist\launcher.toml` to change the monitor config file name or the refresh intervals, then
restart `aster-launcher.exe` to apply changes.

Each process's own output goes to `dist\logs\aster-sysinfo.log`, `dist\logs\asterctl.log`, and
`dist\logs\hwbridge.log`. If a process crashes while the launcher is running, it's automatically
restarted and a marker line is appended to its log.
```

- [ ] **Step 5: Verify the script runs end-to-end on Windows**

Prerequisite: `cargo build --release` and `HwBridge.exe` built, as in Task 8 Step 3.

Run (on Windows, from repo root): `.\windows\package-dist.ps1`
Expected: no errors; `dist\aster-launcher.exe`, `dist\bin\aster-sysinfo.exe`, `dist\bin\asterctl.exe`, `dist\hwbridge\HwBridge.exe` (+ DLLs), `dist\cfg\...`, `dist\launcher.toml` all exist. Then repeat the Task 8 Step 3 manual verification, this time double-clicking `dist\aster-launcher.exe` directly (no hand-copying needed).

- [ ] **Step 6: Commit**

```bash
git add windows/package-dist.ps1 windows/launcher.default.toml docs/windows/README.md
git add -u windows/start-services.ps1
git commit -m "feat(windows): add package-dist.ps1, drop start-services.ps1"
```
