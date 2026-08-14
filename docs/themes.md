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
