# Themes Menu Implementation Plan

> **For agentic workers:** implement this plan task-by-task — dispatch a fresh subagent per task with the native `task` tool (recommended for quality), or use the superpowers-executing-plans skill to work through it inline. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Windows tray "Themes" sub-menu (Default / Cyberpunk / Interstellar / Cartoon) plus an `asterctl --theme <0-3>` flag that switches the LCD panel theme with official AOOSTAR-X semantics.

**Architecture:** asterctl becomes theme-aware: `setup.theme` (or the `--theme` flag, which wins) selects the active built-in panel pair via the official `active_panels_for_theme` mapping, replacing the config's `mianban` when valid. The launcher persists `theme` in `launcher.toml` (mirroring the existing `refresh_time` pattern), passes it to asterctl as `--theme`, and restarts asterctl from a new tray "Themes" sub-menu when the user picks one.

**Tech Stack:** Rust (edition 2024), clap 4 derive, serde/serde_json, tray-item (Windows), tempfile (dev-deps).

**Spec:** `docs/reasonix/specs/2026-08-13-themes-menu-design.md`

**Environment notes (WSL):** all cargo test/check commands run in the Debian docker container; asterctl builds need `pkg-config libudev-dev` in the container's apt install line (added to the commands below). If `git add`/`git commit` fails with `.git/index.lock` errors (read-only `/mnt/c` mount for new files), fall back to Windows Git: `GITEXE="/mnt/c/Users/huynh/scoop/apps/git/current/cmd/git.exe" && "$GITEXE" -c safe.directory='*' -c commit.gpgSign=false -C "C:/Users/huynh/aoostar-rs" commit -m "<msg>"`. `crates/aster-launcher/src/tray.rs:210` has a pre-existing rustfmt diff (multi-line `append_line` call) — Task 7 must run rustfmt on tray.rs so the repo ends fmt-clean.

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `crates/asterctl/src/cfg.rs` | AOOSTAR-X config format; theme→panel mapping | Add `Setup` fields, `active_panels_for_theme`, `MonitorConfig::apply_theme`, tests |
| `crates/asterctl/src/main.rs` | CLI + config loading | Add `--theme` flag, wire into `load_configuration`, tests |
| `crates/asterctl/Cargo.toml` | deps | Add `tempfile = "3"` dev-dependency |
| `crates/aster-launcher/src/config.rs` | launcher.toml parsing/persisting | Add `THEME_OPTIONS`, `theme` field, `set_theme`, sanitize, tests |
| `crates/aster-launcher/src/process.rs` | child process specs | Pass `--theme N` to asterctl; tests |
| `crates/aster-launcher/src/main.rs` | launcher startup | `current_theme` atomic, pass to `tray::run` |
| `crates/aster-launcher/src/tray.rs` | tray menu (Windows-only) | "Themes" sub-menu + `apply_theme` handler |
| `CHANGELOG.md` | changelog | Unreleased → Added entry |
| `docs/themes.md` | theme documentation | New doc: mapping, provenance, extension guide |

---

### Task 1: asterctl — `active_panels_for_theme` mapping

**Files:**
- Modify: `crates/asterctl/src/cfg.rs` (append `#[cfg(test)] mod tests` at end of file)

- [ ] **Step 1: Write the failing test**

Append to `crates/asterctl/src/cfg.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_maps_panels_exactly_like_official_software() {
        // (theme, controlParams, controlDiskTemp) -> expected active panels
        let cases: &[(i32, bool, bool, &[u32])] = &[
            (0, true, true, &[1, 2]),
            (0, true, false, &[1]),
            (0, false, true, &[2]),
            (0, false, false, &[]),
            (1, true, true, &[3, 4]),
            (1, true, false, &[3]),
            (1, false, true, &[4]),
            (1, false, false, &[]),
            (2, true, true, &[5, 6]),
            (2, true, false, &[5]),
            (2, false, true, &[6]),
            (2, false, false, &[]),
            (3, true, true, &[7, 8]),
            (3, true, false, &[7]),
            (3, false, true, &[8]),
            (3, false, false, &[]),
        ];
        for (theme, params, disk, expected) in cases {
            assert_eq!(
                active_panels_for_theme(*theme, *params, *disk),
                *expected,
                "theme={theme} controlParams={params} controlDiskTemp={disk}"
            );
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p asterctl theme_maps_panels"`
Expected: FAIL with `error[E0425]: cannot find function active_panels_for_theme in this scope`. (Note: asterctl builds libudev-sys, so the container needs `pkg-config libudev-dev`.)

- [ ] **Step 3: Write minimal implementation**

In `crates/asterctl/src/cfg.rs`, insert right before `impl MonitorConfig {` (after the `MonitorConfig` struct definition):

```rust
/// Maps a theme index (0-3) plus the two panel-type flags to the 1-based
/// active panel indices, mirroring the official AOOSTAR-X save-config logic:
///
/// | theme | both flags     | controlParams only | controlDiskTemp only | neither |
/// |-------|----------------|--------------------|----------------------|---------|
/// | 0     | [1, 2]         | [1]                | [2]                  | []      |
/// | 1     | [3, 4]         | [3]                | [4]                  | []      |
/// | 2     | [5, 6]         | [5]                | [6]                  | []      |
/// | 3     | [7, 8]         | [7]                | [8]                  | []      |
pub fn active_panels_for_theme(
    theme: i32,
    control_params: bool,
    control_disk_temp: bool,
) -> Vec<u32> {
    let first = (1 + 2 * theme) as u32;
    let second = first + 1;
    match (control_params, control_disk_temp) {
        (true, true) => vec![first, second],
        (true, false) => vec![first],
        (false, true) => vec![second],
        (false, false) => vec![],
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p asterctl theme_maps_panels"`
Expected: PASS (16 assertions).

- [ ] **Step 5: Commit**

```bash
git add crates/asterctl/src/cfg.rs
git commit -m "feat(asterctl): add official theme-to-panels mapping"
```

---

### Task 2: asterctl — `Setup` theme fields + `MonitorConfig::apply_theme`

**Files:**
- Modify: `crates/asterctl/src/cfg.rs`

- [ ] **Step 1: Write the failing tests**

Inside the `mod tests` block added in Task 1, add this helper plus tests:

```rust
    /// Builds a config with `n` minimal panels. `theme` is only written into
    /// `setup` when `Some`. The panel-type flags are always present.
    fn config_with_panels(n: usize, theme: Option<i32>, params: bool, disk: bool) -> MonitorConfig {
        let panels: Vec<String> = (1..=n)
            .map(|i| {
                format!(
                    r#"{{"img": "default_{i}_index.jpg", "sensor": [{{"mode": 1, "label": "cpu", "value": "", "unit": "", "integerDigits": -1, "decimalDigits": -1, "pic": "", "x": 10, "y": 10}}]}}"#
                )
            })
            .collect();
        let theme_json = theme
            .map(|t| format!(r#", "theme": {t}"#))
            .unwrap_or_default();
        let json = format!(
            r#"{{"setup": {{"refresh": 1, "controlParams": {params}, "controlDiskTemp": {disk}{theme_json}}}, "mianban": [1], "diy": [{}]}}"#,
            panels.join(",")
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn apply_theme_replaces_active_panels() {
        let mut cfg = config_with_panels(8, Some(2), true, true);
        assert_eq!(cfg.active_panels, vec![1]);
        cfg.apply_theme(2);
        assert_eq!(cfg.active_panels, vec![5, 6]);
    }

    #[test]
    fn apply_theme_honors_panel_type_flags() {
        let mut cfg = config_with_panels(8, Some(1), true, false);
        cfg.apply_theme(1);
        assert_eq!(cfg.active_panels, vec![3]);
    }

    #[test]
    fn apply_theme_keeps_mianban_when_panels_are_missing() {
        let mut cfg = config_with_panels(4, Some(3), true, true); // theme 3 wants [7, 8]
        cfg.apply_theme(3);
        assert_eq!(cfg.active_panels, vec![1]);
    }

    #[test]
    fn apply_theme_keeps_mianban_for_out_of_range_theme() {
        let mut cfg = config_with_panels(8, None, true, true);
        cfg.apply_theme(7);
        assert_eq!(cfg.active_panels, vec![1]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p asterctl apply_theme"`
Expected: FAIL with `error[E0599]: no method named apply_theme found for struct MonitorConfig` (and `error[E0609]: no field theme on type Setup`).

- [ ] **Step 3: Implement**

In `crates/asterctl/src/cfg.rs`, inside the `Setup` struct, replace the commented-out block:

```rust
    /// Default: true
    pub off_display: bool,
    /// Selection of default panels based on theme / control_params / control_disk_temp ?
    pub theme: i32,
    /// ? Default: true
    pub control_params: bool,
    /// ? Default: true
    pub control_disk_temp: bool,
```

with nothing (delete those 8 comment/field lines), and insert after `pub refresh: f32,`:

```rust
    /// Theme index 0-3 (Default, Cyberpunk, Interstellar, Cartoon): selects
    /// which built-in panels are active, exactly like the official
    /// AOOSTAR-X software. Absent in custom configs.
    pub theme: Option<i32>,
    /// Show the "index" (system overview) panel of the theme's panel pair.
    pub control_params: Option<bool>,
    /// Show the "hdd" (disk temperature) panel of the theme's panel pair.
    pub control_disk_temp: Option<bool>,
```

(`#[serde(rename_all = "camelCase")]` on `Setup` already maps `control_params` → `controlParams` and `control_disk_temp` → `controlDiskTemp`; `Option` fields default to `None` when absent.)

Then add this method to `impl MonitorConfig` (next to `get_next_active_panel`):

```rust
    /// Applies a theme index: replaces `active_panels` with the panel pair
    /// the official AOOSTAR-X software would activate for this theme,
    /// honoring the `controlParams` / `controlDiskTemp` flags.
    ///
    /// Keeps the config's own `mianban` unchanged (with a logged warning)
    /// when the theme is out of range or the config does not define all
    /// referenced panels.
    pub fn apply_theme(&mut self, theme: i32) {
        if !(0..=3).contains(&theme) {
            warn!("Invalid theme {theme}, expected 0-3; keeping the configured active panels");
            return;
        }
        let panels = active_panels_for_theme(
            theme,
            self.setup.control_params.unwrap_or(false),
            self.setup.control_disk_temp.unwrap_or(false),
        );
        if panels.iter().all(|&p| (p as usize) <= self.panels.len()) {
            self.active_panels = panels;
        } else {
            warn!(
                "Theme {theme} selects panels {panels:?}, but the config defines only {} panels; keeping the configured active panels",
                self.panels.len()
            );
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p asterctl"`
Expected: PASS (all asterctl tests, including the Task 1 ones).

- [ ] **Step 5: Commit**

```bash
git add crates/asterctl/src/cfg.rs
git commit -m "feat(asterctl): honor setup.theme and apply theme panel set"
```

---

### Task 3: asterctl — `--theme` CLI flag

**Files:**
- Modify: `crates/asterctl/Cargo.toml`
- Modify: `crates/asterctl/src/main.rs`

- [ ] **Step 1: Write the failing tests**

Add `tempfile = "3"` to `crates/asterctl/Cargo.toml` dev-dependencies (next to `rstest`).

Append a `#[cfg(test)] mod tests` block at the end of `crates/asterctl/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_config(dir: &std::path::Path, theme: Option<i32>, mianban: &[u32]) {
        let panels: Vec<String> = (1..=8)
            .map(|i| {
                format!(
                    r#"{{"img": "default_{i}_index.jpg", "sensor": [{{"mode": 1, "label": "cpu", "value": "", "unit": "", "integerDigits": -1, "decimalDigits": -1, "pic": "", "x": 10, "y": 10}}]}}"#
                )
            })
            .collect();
        let theme_json = theme
            .map(|t| format!(r#", "theme": {t}"#))
            .unwrap_or_default();
        let mianban_json: Vec<String> = mianban.iter().map(|p| p.to_string()).collect();
        let json = format!(
            r#"{{"setup": {{"refresh": 1, "controlParams": true, "controlDiskTemp": true{theme_json}}}, "mianban": [{}], "diy": [{}]}}"#,
            mianban_json.join(","),
            panels.join(",")
        );
        fs::write(dir.join("Monitor3.json"), json).unwrap();
    }

    #[test]
    fn cli_theme_overrides_json_theme() {
        let dir = tempdir().unwrap();
        write_config(dir.path(), Some(2), &[1]);
        let cfg = load_configuration(
            "Monitor3.json",
            dir.path(),
            None,
            dir.path().join("no-such-mapping.cfg"),
            Some(3),
        )
        .unwrap();
        assert_eq!(cfg.active_panels, vec![7, 8]);
    }

    #[test]
    fn json_theme_applies_when_no_cli_flag() {
        let dir = tempdir().unwrap();
        write_config(dir.path(), Some(1), &[1]);
        let cfg = load_configuration(
            "Monitor3.json",
            dir.path(),
            None,
            dir.path().join("no-such-mapping.cfg"),
            None,
        )
        .unwrap();
        assert_eq!(cfg.active_panels, vec![3, 4]);
    }

    #[test]
    fn mianban_untouched_without_theme() {
        let dir = tempdir().unwrap();
        write_config(dir.path(), None, &[1]);
        let cfg = load_configuration(
            "Monitor3.json",
            dir.path(),
            None,
            dir.path().join("no-such-mapping.cfg"),
            None,
        )
        .unwrap();
        assert_eq!(cfg.active_panels, vec![1]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p asterctl cli_theme_overrides_json_theme"`
Expected: FAIL with `error[E0061]: this function takes 4 arguments but 5 were supplied` for `load_configuration`.

- [ ] **Step 3: Implement**

In `crates/asterctl/src/main.rs`:

1. Add the flag to `Args`, after the `--shm` argument:

```rust
    /// Theme index 0-3 (Default, Cyberpunk, Interstellar, Cartoon).
    /// Overrides the `theme` value in the config file's `setup` section.
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=3))]
    theme: Option<u32>,
```

2. Change `load_configuration` to take separate path generics and the theme flag:

```rust
fn load_configuration<P: AsRef<Path>, Q: AsRef<Path>, R: AsRef<Path>>(
    config: P,
    config_dir: Q,
    panels: Option<Vec<PathBuf>>,
    sensor_mapping: R,
    theme: Option<u32>,
) -> anyhow::Result<MonitorConfig> {
    let config = config.as_ref();
    let config_dir = config_dir.as_ref();

    let mut cfg = if config.is_absolute() {
        cfg::load_cfg(config)?
    } else {
        cfg::load_cfg(config_dir.join(config))?
    };

    // Theme selection (official AOOSTAR-X parity): the CLI flag wins over
    // the JSON's `setup.theme`; when neither is present, the config's own
    // `mianban` is left untouched (backward compatible for custom configs).
    if let Some(theme) = theme.map(|t| t as i32).or(cfg.setup.theme) {
        cfg.apply_theme(theme);
    }

    if let Some(panels) = panels {
        for panel in panels {
            cfg.include_custom_panel(load_custom_panel(panel)?);
        }
    }
    // ... rest unchanged
```

3. Update the call site in `main()`:

```rust
        let cfg = load_configuration(&config, &cfg_dir, args.panels, &mapping_cfg, args.theme)?;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p asterctl"`
Expected: PASS (3 new tests + all previous).

- [ ] **Step 5: Commit**

```bash
git add crates/asterctl/Cargo.toml crates/asterctl/src/main.rs
git commit -m "feat(asterctl): add --theme CLI flag"
```

---

### Task 4: aster-launcher — `theme` config key + `set_theme`

**Files:**
- Modify: `crates/aster-launcher/src/config.rs`

- [ ] **Step 1: Write the failing tests**

Inside the existing `mod tests` in `crates/aster-launcher/src/config.rs`, add:

```rust
    #[test]
    fn set_theme_updates_value_and_keeps_comments_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.toml");
        std::fs::write(&path, "# my config\ntheme = 0\n\nrefresh_time = 5\n").unwrap();

        set_theme(&path, 2).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("theme = 2"));
        assert!(!text.contains("theme = 0"));
        assert!(text.contains("refresh_time = 5"));
    }

    #[test]
    fn set_theme_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.toml");

        set_theme(&path, 3).unwrap();

        let cfg = LauncherConfig::load(&path);
        assert_eq!(cfg.theme, Some(3));
    }

    #[test]
    fn invalid_theme_is_ignored() {
        let file = write_temp("theme = 9\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.theme, None);
    }

    #[test]
    fn valid_theme_is_kept() {
        let file = write_temp("theme = 1\n");
        let cfg = LauncherConfig::load(file.path());
        assert_eq!(cfg.theme, Some(1));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p aster-launcher set_theme"`
Expected: FAIL with `error[E0425]: cannot find function set_theme in this scope` / `error[E0609]: no field theme on type LauncherConfig`.

- [ ] **Step 3: Implement**

In `crates/aster-launcher/src/config.rs`:

1. Add the theme options constant after `DEFAULT_REFRESH_SECS`:

```rust
/// Built-in theme options shown in the tray "Themes" sub-menu: (index, label).
/// Names match the official AOOSTAR-X theme dropdown (English).
pub const THEME_OPTIONS: [(u16, &str); 4] = [
    (0, "Default"),
    (1, "Cyberpunk"),
    (2, "Interstellar"),
    (3, "Cartoon"),
];
```

2. Add the field to `LauncherConfig` (after `refresh_time`) and to `Default`:

```rust
    /// LCD theme index 0-3, passed to asterctl via `--theme`. Must be one
    /// of [`THEME_OPTIONS`]. Default: not configured (asterctl then uses the
    /// `theme` value in its monitor config).
    pub theme: Option<u16>,
```

```rust
            refresh_time: None,
            theme: None,
```

3. Replace `sanitize_refresh_values` with a renamed `sanitize_values` that also handles theme (update the call in `load`):

```rust
        cfg.sanitize_values(path);
```

```rust
    /// Drops any refresh value that is not one of [`REFRESH_OPTIONS`] and any
    /// theme that is not one of [`THEME_OPTIONS`], so an out-of-range value
    /// can never reach a child process (e.g. the old `0`-seconds permanent
    /// respawn loop). Invalid values are noted in `launcher.log` and the
    /// field reverts to "not configured", so a bad `refresh_time` still falls
    /// back to the legacy keys / default.
    fn sanitize_values(&mut self, path: &Path) {
        let log = path.with_file_name("launcher.log");
        if let Some(v) = self.refresh_time
            && !REFRESH_OPTIONS.contains(&v)
        {
            crate::logging::append_line(
                &log,
                &format!(
                    "refresh_time={v} is not one of the allowed refresh intervals {REFRESH_OPTIONS:?}; ignoring it"
                ),
            );
            self.refresh_time = None;
        }
        if let Some(v) = self.theme
            && !THEME_OPTIONS.iter().any(|(idx, _)| *idx == v)
        {
            crate::logging::append_line(
                &log,
                &format!(
                    "theme={v} is not one of the allowed themes {THEME_OPTIONS:?}; ignoring it"
                ),
            );
            self.theme = None;
        }
        // ... existing sysinfo_refresh / hwbridge_refresh checks unchanged
    }
```

4. Replace `set_refresh_time` with a generic writer plus two public wrappers:

```rust
/// Rewrites a `key = <N>` line in the TOML file at `path`, preserving every
/// other line and comment (used by the tray's refresh / themes menus).
/// Creates the file with just that option if it does not exist yet.
/// Does not validate `value` — callers pass one of the allowed options.
fn set_toml_value(path: &Path, key: &str, value: u16) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };

    let line = format!("{key} = {value}");
    let mut rewritten = false;
    let mut out = String::with_capacity(text.len() + line.len() + 2);
    for src in text.split_inclusive('\n') {
        let entry_key = src.trim().split('=').next().unwrap_or("").trim();
        if entry_key == key {
            // keep the original indentation, drop any trailing comment
            let indent: String = src.chars().take_while(|c| c.is_whitespace()).collect();
            out.push_str(&indent);
            out.push_str(&line);
            if !src.ends_with('\n') {
                out.push('\n');
            }
            rewritten = true;
        } else {
            out.push_str(src);
        }
    }
    if !rewritten {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
        out.push('\n');
    }
    std::fs::write(path, out)
}

pub fn set_refresh_time(path: &Path, secs: u16) -> std::io::Result<()> {
    set_toml_value(path, "refresh_time", secs)
}

/// Persists the `theme = <N>` line in `launcher.toml` (see [`set_toml_value`]).
pub fn set_theme(path: &Path, theme: u16) -> std::io::Result<()> {
    set_toml_value(path, "theme", theme)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p aster-launcher"`
Expected: PASS (all config tests incl. the existing `set_refresh_time_*` ones, which now go through `set_toml_value`).

- [ ] **Step 5: Commit**

```bash
git add crates/aster-launcher/src/config.rs
git commit -m "feat(launcher): persist theme in launcher.toml"
```

---

### Task 5: aster-launcher — pass `--theme` to asterctl

**Files:**
- Modify: `crates/aster-launcher/src/process.rs`

- [ ] **Step 1: Write the failing test**

Inside the existing `mod tests` in `crates/aster-launcher/src/process.rs`, add:

```rust
    #[test]
    fn theme_flag_flows_through_to_asterctl_args() {
        let base_dir = Path::new("C:\\dist");
        let cfg = LauncherConfig {
            monitor_config: "Monitor3.json".to_string(),
            refresh_time: None,
            sysinfo_refresh: None,
            hwbridge_refresh: None,
            restart_uart_on_resume: true,
            theme: Some(2),
        };

        let specs = child_specs(base_dir, &cfg);

        assert_eq!(
            specs[1].args,
            vec![
                "--config".to_string(),
                "Monitor3.json".to_string(),
                "--shm".to_string(),
                "--theme".to_string(),
                "2".to_string()
            ]
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p aster-launcher theme_flag_flows_through_to_asterctl_args"`
Expected: FAIL to compile — `error[E0063]: missing field theme in initializer of LauncherConfig` (both this test and the two existing literal constructions in `builds_specs_relative_to_base_dir_using_config_values` / `legacy_refresh_values_flow_through_when_no_shared_refresh_time`).

- [ ] **Step 3: Implement**

In `crates/aster-launcher/src/process.rs`:

1. Add `theme: None,` to both existing `LauncherConfig { ... }` literals in the tests (after `restart_uart_on_resume: true,`).

2. Replace the asterctl `ChildSpec` args in `child_specs`:

```rust
        ChildSpec {
            name: "asterctl",
            base_dir: base_dir.to_path_buf(),
            exe_path: base_dir.join("bin").join("asterctl.exe"),
            args: {
                let mut args = vec![
                    "--config".to_string(),
                    cfg.monitor_config.clone(),
                    "--shm".to_string(),
                ];
                if let Some(theme) = cfg.theme {
                    args.push("--theme".to_string());
                    args.push(theme.to_string());
                }
                args
            },
            log_path: logs_dir.join("asterctl.log"),
        },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p aster-launcher"`
Expected: PASS. The existing `builds_specs_relative_to_base_dir_using_config_values` still asserts asterctl args WITHOUT `--theme` (its literal has `theme: None`).

- [ ] **Step 5: Commit**

```bash
git add crates/aster-launcher/src/process.rs
git commit -m "feat(launcher): pass theme to asterctl via --theme"
```

---

### Task 6: aster-launcher — `current_theme` state

**Files:**
- Modify: `crates/aster-launcher/src/main.rs`

- [ ] **Step 1: Implement (no new unit test — Windows-only wiring)**

In `crates/aster-launcher/src/main.rs`:

1. After `let current_refresh = ...;` add:

```rust
    // Active theme for the tray "Themes" check mark; Default (0) is the
    // effective fallback when launcher.toml does not configure a theme.
    let current_theme = Arc::new(AtomicU16::new(cfg.theme.unwrap_or(0)));
```

2. Update the `tray::run` call (add `current_theme,` after `current_refresh,`):

```rust
    tray::run(
        &handles,
        specs,
        current_refresh,
        current_theme,
        quit.clone(),
        &launcher_log,
        &config_path,
        &base_dir,
        &cfg,
    );
```

- [ ] **Step 2: Verify it still compiles (cross-platform test)**

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo check -p aster-launcher"`
Expected: PASS — but note this only type-checks the non-Windows path; the Windows `tray::run` signature change is checked in Task 7.

- [ ] **Step 3: Commit**

```bash
git add crates/aster-launcher/src/main.rs
git commit -m "feat(launcher): track current theme for the tray menu"
```

---

### Task 7: aster-launcher — tray "Themes" sub-menu (Windows-only)

**Files:**
- Modify: `crates/aster-launcher/src/tray.rs`

- [ ] **Step 1: Implement (Windows-only code, manually verified — no unit test)**

In `crates/aster-launcher/src/tray.rs`:

1. Add the `apply_theme` handler after `apply_refresh`:

```rust
/// Tray "Themes" click handler: persists `theme` to `launcher.toml`,
/// rebuilds the child specs with the new theme, and restarts asterctl so it
/// applies immediately (its watcher reads the specs fresh on every spawn).
/// `current` is updated so the tray menu can move the check mark.
#[cfg(windows)]
fn apply_theme(
    theme: u16,
    config_path: &Path,
    base_dir: &Path,
    cfg: &LauncherConfig,
    specs: &Mutex<[ChildSpec; 3]>,
    handles: &[ChildHandle],
    log_path: &Path,
    current: &AtomicU16,
) {
    // 1. persist the choice so it survives a launcher restart
    if let Err(err) = crate::config::set_theme(config_path, theme) {
        crate::logging::append_line(
            log_path,
            &format!(
                "failed to write theme={theme} to {}: {err}",
                config_path.display()
            ),
        );
        return;
    }
    crate::logging::append_line(
        log_path,
        &format!("tray: theme set to {theme}; restarting asterctl"),
    );

    // 2. rebuild the specs with the new theme
    let mut new_cfg = cfg.clone();
    new_cfg.theme = Some(theme);
    if let Ok(mut guard) = specs.lock() {
        *guard = crate::process::child_specs(base_dir, &new_cfg);
    }

    // 3. kill asterctl; its watcher respawns it with the updated args
    crate::process::kill_named(handles, &["asterctl"]);

    current.store(theme, Ordering::SeqCst);
}
```

2. Add `current_theme: Arc<AtomicU16>` to the `run` signature (after `current_refresh`) and update the doc comment's first paragraph to mention the Themes sub-menu:

```rust
pub fn run(
    handles: &[ChildHandle],
    specs: Arc<Mutex<[ChildSpec; 3]>>,
    current_refresh: Arc<AtomicU16>,
    current_theme: Arc<AtomicU16>,
    quit: Arc<AtomicBool>,
    log_path: &Path,
    config_path: &Path,
    base_dir: &Path,
    cfg: &LauncherConfig,
) {
```

3. After the "Refresh time" sub-menu block and before the "Quit" item, add the Themes sub-menu (same shape as the refresh one):

```rust
    // "Themes" sub-menu: picking a theme persists it to launcher.toml and
    // restarts asterctl so it applies immediately (the watcher respawns it
    // with the new `--theme` argument). The active theme carries a check mark.
    let active_theme = current_theme.load(Ordering::SeqCst);
    let mut theme_submenu: Option<u32> = None;
    let mut theme_ids = [0u32; crate::config::THEME_OPTIONS.len()];
    match tray.inner_mut().add_submenu("Themes") {
        Ok(sub) => {
            theme_submenu = Some(sub);
            for (i, (idx, label)) in crate::config::THEME_OPTIONS.iter().enumerate() {
                let idx = *idx;
                let config_path = config_path.to_path_buf();
                let base_dir = base_dir.to_path_buf();
                let cfg = cfg.clone();
                let specs = specs.clone();
                let handles = handles.to_vec();
                let log_path = log_path.to_path_buf();
                // separate clone for the error arm below (the original is
                // moved into the menu closure)
                let log_path_err = log_path.clone();
                let current = current_theme.clone();
                let label = label.to_string();
                match tray
                    .inner_mut()
                    .add_submenu_item_with_id(sub, &label, move || {
                        apply_theme(
                            idx,
                            &config_path,
                            &base_dir,
                            &cfg,
                            &specs,
                            &handles,
                            &log_path,
                            &current,
                        );
                    }) {
                    Ok(id) => theme_ids[i] = id,
                    Err(err) => crate::logging::append_line(
                        &log_path_err,
                        &format!("failed to add tray theme menu item ({label}): {err}"),
                    ),
                }
            }
            // Check the currently active theme.
            if let Some(pos) =
                crate::config::THEME_OPTIONS
                    .iter()
                    .position(|&(idx, _)| idx == active_theme)
            {
                let _ = tray
                    .inner_mut()
                    .set_submenu_item_checked(sub, theme_ids[pos], true);
            }
        }
        Err(err) => crate::logging::append_line(
            log_path,
            &format!("failed to add tray Themes submenu: {err}"),
        ),
    }
```

4. Update the 2-second loop to move the theme check mark when it changes (add after the refresh check-mark update):

```rust
        // Move the check mark when the theme changed (e.g. the user picked a
        // different one from the "Themes" submenu).
        let theme = current_theme.load(Ordering::SeqCst);
        if theme != last_theme {
            if let Some(sub) = theme_submenu {
                for (i, id) in theme_ids.iter().enumerate() {
                    if *id != 0
                        && let Err(err) = tray.inner_mut().set_submenu_item_checked(
                            sub,
                            *id,
                            crate::config::THEME_OPTIONS[i].0 == theme,
                        )
                    {
                        crate::logging::append_line(
                            log_path,
                            &format!("failed to update theme menu check mark: {err}"),
                        );
                        break;
                    }
                }
            }
            last_theme = theme;
        }
```

5. Declare `let mut last_theme = active_theme;` next to `let mut last_refresh = active_refresh;`.

- [ ] **Step 2: Type-check the Windows code path**

Run: `./build-from-wsl.sh windows-check`
Expected: PASS — `cargo check -p aster-launcher --target x86_64-pc-windows-msvc` compiles `tray.rs` (this is the verification that the Windows-only menu code is correct; runtime behavior is manually verified on Windows per project convention).

- [ ] **Step 3: Commit**

```bash
git add crates/aster-launcher/src/tray.rs
git commit -m "feat(launcher): add tray Themes sub-menu"
```

---

### Task 8: Documentation

**Files:**
- Modify: `CHANGELOG.md`
- Create: `docs/themes.md`

- [ ] **Step 1: Add the CHANGELOG entry**

Under `## Unreleased` → `### Added`, append:

```markdown
- `aster-launcher` tray "Themes" sub-menu with the official AOOSTAR-X theme options (Default, Cyberpunk,
  Interstellar, Cartoon): picking one persists `theme` in `launcher.toml` and restarts `asterctl`, which
  activates the matching built-in panel pair. `asterctl --theme <0-3>` selects the theme from the command
  line (the CLI flag wins over `setup.theme` in the monitor config; without either, the config's own
  `mianban` is used unchanged). Theme handling mirrors the official AOOSTAR-X behavior: theme N activates
  panels `(1+2N, 2+2N)` filtered by the `controlParams` / `controlDiskTemp` flags.
```

- [ ] **Step 2: Create `docs/themes.md`**

```markdown
# Themes

The LCD panel theme is selected either from the `aster-launcher` tray menu
("Themes") or with `asterctl --theme <0-3>`. The CLI flag wins over the
`theme` value in the monitor config's `setup` section; when neither is set,
the config's own `mianban` (active panels) is used unchanged.

## How a theme maps to panels

Mirrors the official AOOSTAR-X software exactly (reverse-engineered from the
installed `AOOSTAR-X.exe` save-config logic):

| theme | name        | panels when both flags | `controlParams` only | `controlDiskTemp` only |
|-------|-------------|------------------------|----------------------|------------------------|
| 0     | Default     | 1, 2                   | 1                    | 2                      |
| 1     | Cyberpunk   | 3, 4                   | 3                    | 4                      |
| 2     | Interstellar| 5, 6                   | 5                    | 6                      |
| 3     | Cartoon     | 7, 8                   | 7                    | 8                      |

Panels are 1-based indices into `diy` in `Monitor3.json`; the "index" panel
shows system overview sensors, the "hdd" panel shows disk temperature
sensors. If a theme references panels the config does not define, the
config's own `mianban` is kept and a warning is logged.

## Persistence

- Tray menu / `launcher.toml`: `theme = <0-3>` (shared with `aster-launcher`,
  passed to asterctl as `--theme`).
- Monitor config: `setup.theme` in `Monitor3.json` (kept for compatibility
  with the official software; not rewritten by the launcher).

## Images

`cfg/` contains the 10 official `sys_img` files (`default_1..4_index/hdd.jpg`,
`progress1.png`, `progress2.png`), copied byte-identical from
`C:\Program Files (x86)\AOOSTAR-X\_internal\sys_img`. The official software
ships only the Default theme's art; all 4 "themes" reuse it (each activates a
different panel layout). To add real theme art later, drop
`<theme-prefix>_N_index.jpg` / `<theme-prefix>_N_hdd.jpg` into `cfg/`, add
panels referencing them to `Monitor3.json`, and extend
`active_panels_for_theme` in `crates/asterctl/src/cfg.rs` — the tray menu
already supports any option in `THEME_OPTIONS`
(`crates/aster-launcher/src/config.rs`).
```

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md docs/themes.md
git commit -m "docs(themes): document theme selection and mapping"
```

---

### Task 9: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: PASS (repo must stay fmt-clean).

- [ ] **Step 2: Full test suite (launcher + sysinfo + asterctl)**

Run: `./build-from-wsl.sh test`
Expected: PASS for `aster-launcher` and `aster-sysinfo`.

Then run the asterctl tests in the container (the script's `test` action does
not cover asterctl):

Run: `docker run --rm -v "$PWD:/work" -w /work -e CARGO_TARGET_DIR=/tmp/target debian:bookworm-slim bash -lc "apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq gcc curl pkg-config libudev-dev >/dev/null 2>&1 && curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97 >/dev/null 2>&1 && export PATH=\$HOME/.cargo/bin:\$PATH && cd /work && cargo test -p asterctl"`
Expected: PASS (theme mapping, apply_theme, and CLI precedence tests).

- [ ] **Step 3: Windows type-check**

Run: `./build-from-wsl.sh windows-check`
Expected: PASS (`tray.rs` + `process.rs` Windows code compiles).

- [ ] **Step 4: Manual Windows verification (deployable, per project convention)**

On Windows: `windows/package-dist.ps1`, then run `aster-launcher.exe`, open
the tray "Themes" sub-menu, pick each of the 4 themes and confirm:
- the check mark moves to the selection;
- `launcher.toml` gets `theme = <N>`;
- asterctl restarts (~2s) and the LCD shows the theme's panel pair
  (`default_1..4` layouts);
- picking the same theme again does not restart asterctl unnecessarily
  (only the 4 distinct values trigger a restart).

- [ ] **Step 5: Final commit (if verification produced changes)**

```bash
git add -A
git commit -m "chore: final verification fixes" || true
```

---

## Self-Review

**1. Spec coverage:**
- Setup `theme`/`controlParams`/`controlDiskTemp` fields + `active_panels_for_theme` + `apply_theme` fallback → Tasks 1-2.
- `--theme` flag, CLI > JSON precedence, `mianban` untouched when absent → Task 3.
- `THEME_OPTIONS`, `theme` field, `set_theme`, sanitize → Task 4.
- `--theme N` in asterctl args → Task 5.
- `current_theme` atomic + tray wiring → Tasks 6-7.
- Themes tray sub-menu with check marks + `apply_theme` (persist → rebuild specs → kill asterctl) → Task 7.
- cfg.rs 16-combo tests, apply_theme fallback tests, config.rs set_theme tests, process.rs arg test → Tasks 1, 2, 4, 5.
- CHANGELOG + docs/themes.md → Task 8.
- windows-check for tray.rs → Tasks 7 & 9.
- No spec item left without a task. ✓

**2. Placeholder scan:** Every step has concrete code or exact commands with expected output; no "TBD"/"TODO"/"add error handling" placeholders. ✓

**3. Type consistency:**
- `active_panels_for_theme(i32, bool, bool) -> Vec<u32>` — same signature in Task 1 (definition) and Task 2 (call in `apply_theme`). ✓
- `apply_theme(&mut self, theme: i32)` — defined Task 2, called Task 3 with `theme.map(|t| t as i32).or(cfg.setup.theme)` (Option<i32>). ✓
- `load_configuration<P, Q, R>(config: P, config_dir: Q, panels, sensor_mapping: R, theme: Option<u32>)` — signature Task 3 matches call sites in `main()` and the tests. ✓
- `set_theme(path: &Path, theme: u16) -> std::io::Result<()>` — Task 4 definition, Task 7 call in `apply_theme` (theme: u16). ✓
- `LauncherConfig.theme: Option<u16>` — Task 4 field, Task 5 (`cfg.theme`), Task 6 (`cfg.theme.unwrap_or(0)`), Task 7 (`new_cfg.theme = Some(theme)`). ✓
- `THEME_OPTIONS: [(u16, &str); 4]` — Task 4 definition; Task 7 uses `.iter().enumerate()` (i, (idx, label)), `.position(|&(idx, _)| idx == active_theme)` with `active_theme: u16`, and `THEME_OPTIONS[i].0` — all consistent with the tuple type. ✓
- `tray::run` signature: `current_theme: Arc<AtomicU16>` inserted after `current_refresh` — Task 6 call site and Task 7 definition match. ✓
- `apply_theme` handler name (launcher tray) vs `MonitorConfig::apply_theme` (asterctl) — distinct modules, no conflict; both documented. ✓

Fix applied inline: none needed after review.
