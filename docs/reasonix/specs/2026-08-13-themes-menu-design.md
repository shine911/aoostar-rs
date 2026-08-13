# Themes menu: design

- Date: 2026-08-13
- Branch: `feat/themes`
- Status: approved (2026-08-13), pending spec review

## Goal

Add a "Themes" menu to the Windows launcher tray with theme options
(Default, Cyberpunk, Interstellar, Cartoon) and let the user change the LCD
panel theme from it. Also add a `--theme` CLI flag to `asterctl` so the
selection works for standalone/Linux use.

## Background: how the official AOOSTAR-X software implements themes

Reverse-engineered from the installed `C:\Program Files (x86)\AOOSTAR-X`
(`AOOSTAR-X.exe` PyInstaller archive, `Monitor3.json`, `static/web/config.html`):

- The web UI has a theme dropdown with 4 options, stored as `setup.theme`
  (integer 0-3) in `Monitor3.json`:

  | index | name (EN)  | name (官方) |
  |-------|------------|------------|
  | 0     | Default    | 默认主题   |
  | 1     | Cyberpunk  | 赛博朋克   |
  | 2     | Interstellar | 星际穿越 |
  | 3     | Cartoon    | 卡通像素   |

- The theme selects which built-in panels are active (`mianban`). Official
  save-config logic (`main.config` in the bytecode):

  ```python
  base_pair = (1 + 2*theme, 2 + 2*theme)   # theme 0 -> [1,2], 1 -> [3,4], 2 -> [5,6], 3 -> [7,8]
  if controlParams and controlDiskTemp: mianban = [both]
  elif controlParams:                    mianban = [first]
  elif controlDiskTemp:                  mianban = [second]
  else:                                  mianban = []
  ```

- The installed official software ships **only the Default theme art**
  (`sys_img/default_1..4_index/hdd.jpg`, `progress1.png`, `progress2.png`).
  The 4 "themes" are the 4 built-in panel layouts, all sharing `default_*`
  art. The repo's `cfg/` already contains these 10 files byte-identical
  (verified).

## Decisions (user-approved)

1. **Theme semantics = official parity**: a theme is a panel set. asterctl
   honors `setup.theme` and computes `active_panels` exactly like the
   official app. No new image assets are needed.
2. **Menu scope = tray submenu + CLI flag**: Windows tray "Themes"
   submenu (persists to `launcher.toml`, restarts asterctl) and an
   `asterctl --theme <0-3>` flag.

## Components

### 1. asterctl (`crates/asterctl`)

`cfg.rs`:

- Add `theme: Option<i32>` to `Setup` (field already present in official
  JSON; optional for older/custom configs).
- Add `control_params: Option<bool>` (`controlParams`) and
  `control_disk_temp: Option<bool>` (`controlDiskTemp`) to `Setup`; the
  official theme mapping depends on them.
- New pure function
  `active_panels_for_theme(theme: i32, control_params: bool, control_disk_temp: bool) -> Vec<u32>`
  implementing the official mapping table above.
- New method `MonitorConfig::apply_theme(&mut self, theme: i32)`: computes
  the panels; if all referenced panels exist in `self.panels`, replaces
  `active_panels`; otherwise keeps the config's own `mianban` and logs a
  warning (guards configs with fewer than 8 panels).

`main.rs`:

- New CLI flag `--theme <0-3>` (`u32`, clap, with
  `value_parser = clap::value_parser!(u32).range(0..=3)` so out-of-range
  values are rejected at parse time).
- In `load_configuration`: effective theme = `--theme` if given, else
  `cfg.setup.theme`. If an effective theme exists, call
  `cfg.apply_theme(effective_theme)`. If neither is present, `mianban` is
  left untouched (backward compatible for custom configs). `--panels`
  custom panels are appended after theme application, so they stay active.
- `run_sensor_panel` unchanged.

### 2. aster-launcher (`crates/aster-launcher`)

`config.rs`:

- `THEME_OPTIONS: [(u16, &str); 4]` = `(0, "Default"), (1, "Cyberpunk"),
  (2, "Interstellar"), (3, "Cartoon")` (official English names).
- `pub theme: Option<u16>` on `LauncherConfig`; `sanitize_theme` drops
  out-of-range values (log + `None`), same pattern as
  `sanitize_refresh_values`.
- `set_theme(path, theme) -> std::io::Result<()>` line-rewriter mirroring
  `set_refresh_time` (preserves comments and other keys, appends when
  absent, creates file when missing).

`process.rs`:

- `child_specs` appends `--theme N` to the asterctl `args` when
  `cfg.theme` is set (after `--shm`).

`tray.rs` (Windows-only):

- "Themes" submenu in the same pattern as "Refresh time": 4 items, native
  check mark on the active theme, `apply_theme` click handler that:
  1. persists `theme = N` to `launcher.toml` via `set_theme`,
  2. rebuilds the child specs with the updated config,
  3. `kill_named(handles, &["asterctl"])` — the watcher respawns asterctl
     with the new `--theme` flag within ~2s,
  4. updates the `AtomicU16`/menu check mark.
- Mirror the existing never-panic contract: every failure is logged to
  `launcher.log`, no `Err` escapes.

## Data flow & precedence

```
launcher.toml  theme = N  →  asterctl --theme N  →  overrides Monitor3.json setup.theme
                                                 →  computes active_panels (mianban)
                                                 →  existing render loop
```

- CLI flag wins over the JSON `setup.theme`. Effective theme resolution:
  1. `--theme <0-3>` if given;
  2. else `setup.theme` if present in the JSON;
  3. else no theme → the config's own `mianban` is used unchanged
     (backward compatible for custom configs).
  When an effective theme exists, `active_panels` is replaced by the
  computed panel set (with the missing-panel fallback above).
- The launcher never rewrites `Monitor3.json`; `theme` stays there for
  official-app compatibility.

## Assets

No new assets required: `cfg/` already contains all 10 official `sys_img`
files byte-identical. New `docs/themes.md` documents the theme/panel
mapping table, the asset provenance, and how real theme art (e.g. actual
cartoon images) could be added later — the menu structure supports it
without code changes beyond the mapping.

## Testing

- `crates/asterctl/src/cfg.rs` unit tests:
  - `active_panels_for_theme`: all 4 themes × flag combinations
    (16 combos) match the official mapping.
  - `apply_theme` fallback: config with < 8 panels keeps `mianban` and
    does not panic; config with 8 panels gets the theme panels.
  - theme absent → `mianban` untouched.
- `crates/aster-launcher/src/config.rs` unit tests:
  - `set_theme` updates value, keeps comments/other keys, appends when
    absent, creates file when missing (mirror `set_refresh_time` tests).
  - `sanitize_theme`: out-of-range → `None` + logged.
- `crates/aster-launcher/src/process.rs` unit test:
  - asterctl args contain `--theme N` when set, absent when `None`.
- Tray menu is Windows-only: manually verified per project convention
  (unit tests must stay cross-platform).
- Build: `./build-from-wsl.sh test` then `windows-build` on Windows
  (launcher needs an elevated shell).

## Docs & changelog

- `CHANGELOG.md`: new entry under Unreleased.
- `docs/themes.md`: mapping table, provenance, extension guide.
- `TODO.md`: no changes expected.

## Out of scope

- Real theme art (cartoon/cyberpunk/interstellar images) — not shipped by
  the installed official software; the design leaves room to add it.
- Rewriting `Monitor3.json` from the launcher.
- `theme` in the asterctl CLI beyond the index flag (no `--control-*`
  flags; control flags come from the JSON setup).
