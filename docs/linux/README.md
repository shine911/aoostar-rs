# AOOSTAR LCD on KDE Plasma

The Linux launcher supports KDE Plasma 5 and 6 on both X11 and Wayland. It
starts `aster-sysinfo` and `asterctl` as a per-user tray application; it does
not install or enable an autostart service. Install the DEB or RPM, then launch
“AOOSTAR LCD” from the KDE application launcher (or run
`/usr/bin/aster-launcher`). No root shell is required.

## Build and install

On a Debian-like development system, install Rust, `cargo-deb`, and
`cargo-generate-rpm`, then run `./linux/package.sh all`. Packages are written
to `dist-linux/`. The WSL helper can build both packages in its Debian
container with `./build-from-wsl.sh linux-packages`.

The package installs immutable assets in `/usr/share/aoostar-rs`, child
executables in `/usr/lib/aoostar-rs`, the launcher at `/usr/bin/aster-launcher`,
and a KDE/freedesktop desktop entry and hicolor icon. It also installs a udev
rule for USB ID `0416:90a1`; unplug and reconnect the LCD after installation so
the rule applies. If access is still denied, check that the session has a
working `uaccess` seat and inspect `/dev/tty*` permissions.

## Tray controls and files

The tray menu provides refresh choices (2/5/10/30 seconds), themes (0–3),
Display On/Off, and Quit. “Follow screen state” is deliberately not offered
on Linux because KDE/Wayland has no portable event API; an old configuration
using it is logged and safely treated as On. If Plasma hides the icon, open the
system tray settings and move AOOSTAR LCD to the visible or expanded area.
Monitor3's CPU, memory, and GPU temperatures plus network speed/address use
stable Linux aliases selected from the active local interface. CPU and RAM
percentages are mapped to the corresponding `aster-sysinfo` usage labels. The
temperature aliases are value-only so Monitor3's own degree symbols are not
duplicated; generic `temperature_*#unit` sensors remain available. GPU
utilization is shown when the kernel exposes a DRM `gpu_busy_percent` attribute
and is otherwise omitted without inventing a value.

Per-user paths follow XDG (with safe fallbacks): configuration is in
`$XDG_CONFIG_HOME/aoostar-rs/launcher.toml`, logs in
`$XDG_STATE_HOME/aoostar-rs/logs/`, and the lock, heartbeat, UART marker, and
sensor directory in `$XDG_RUNTIME_DIR/aoostar-rs/`. `AOOSTAR_*_DIR`
environment overrides are available for development and tests. The launcher
creates all writable directories and never writes under `/usr`.

## Troubleshooting and uninstall

Read `launcher.log`, `asterctl.log`, and `aster-sysinfo.log` in the state logs
directory. A second click is harmless: the per-user lock exits without starting
duplicate children. Quit from the tray before replacing binaries. To remove,
use the package manager (`sudo apt remove aoostar-rs` or `sudo rpm -e
aoostar-rs`), then optionally remove the user directories above. Replug the
LCD after uninstall if another driver should claim its UART.
