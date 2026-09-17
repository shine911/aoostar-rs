# AOOSTAR LCD setup guide

[![Git](https://img.shields.io/github/last-commit/shine911/aoostar-rs?label=git)](https://github.com/shine911/aoostar-rs/commits)
[![Build](https://github.com/shine911/aoostar-rs/actions/workflows/build.yml/badge.svg)](https://github.com/shine911/aoostar-rs/actions/workflows/build.yml)
[![Version](https://img.shields.io/github/v/release/shine911/aoostar-rs?label=version)](https://github.com/shine911/aoostar-rs/releases)

This project controls the secondary LCD in the AOOSTAR WTR MAX and GEM12+ PRO.
It is a fork of the original [zehnm/aoostar-rs](https://github.com/zehnm/aoostar-rs).

> **Hardware warning:** this protocol is reverse engineered. Use it at your own
> risk; a failed display update may require a power cycle.

## Choose an installation method

| Platform | Recommended method | Start command |
| --- | --- | --- |
| Windows 10/11 x64 | Build `dist\` as below | `dist\aster-launcher.exe` |
| Debian/Ubuntu | Build and install the DEB | `aster-launcher` |
| Fedora/RHEL-family | Build and install the RPM | `aster-launcher` |
| Arch Linux | Build `linux/PKGBUILD` with `makepkg` | `aster-launcher` |

On Linux, the launcher is a KDE Plasma 5/6 tray application. It works on X11
and Wayland and does not install an autostart service. The packaged udev rule
grants the logged-in desktop user access to the LCD's `0416:90a1` UART.

## Windows

### Prerequisites

Install these before building:

- Rust 1.88 or newer with the MSVC toolchain, from [rustup](https://rustup.rs/).
- Visual Studio Build Tools with the **Desktop development with C++** workload.
- .NET Framework 4.x (includes `csc.exe`; no .NET SDK is needed).
- The official AOOSTAR-X `PawnIO.exe` prerequisite, if you want CPU/GPU/
  motherboard temperatures and GPU load. The LCD and basic system sensors work
  without it.

### Build and package

Run PowerShell from the repository root:

```powershell
git clone https://github.com/shine911/aoostar-rs.git
cd aoostar-rs
cargo build --release

cd hwbridge
& "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo /r:LibreHardwareMonitorLib.dll /out:HwBridge.exe HwBridge.cs
cd ..

.\windows\package-dist.ps1
```

`dist\` is the complete portable installation: it contains the launcher,
child binaries, `HwBridge.exe`, panel assets, fonts, and `launcher.toml`.
Move it to an Administrator-writable location such as `C:\Program Files\AOOSTAR LCD`.
Do not leave it in a user-writable directory: the launcher and its children run
elevated.

### Run and configure

Start `dist\aster-launcher.exe` and approve the Administrator prompt. It starts
the sensor providers and `asterctl` in the background; use the tray icon to
change refresh time, theme, display state, or quit.

Edit `dist\launcher.toml` before starting to select a panel or set
`refresh_time` to `2`, `5`, `10`, or `30` seconds. Logs are in `dist\logs\`.
Quit the launcher before rebuilding `dist\`.

## Debian and Ubuntu

Install build dependencies and Rust, then build the DEB:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libudev-dev libdbus-1-dev \
  dpkg-dev desktop-file-utils curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

git clone https://github.com/shine911/aoostar-rs.git
cd aoostar-rs
cargo install cargo-deb --locked
./linux/package.sh deb
sudo apt install ./dist-linux/*.deb
```

Unplug and reconnect the LCD once after installation so the udev rule applies.
Then start **AOOSTAR LCD** from the application launcher, or run:

```bash
aster-launcher
```

Remove it with `sudo apt remove aoostar-rs`.

## Fedora and related distributions

Install build dependencies and Rust, then build the RPM:

```bash
sudo dnf install -y gcc pkgconf-pkg-config systemd-devel dbus-devel \
  rpm-build desktop-file-utils curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

git clone https://github.com/shine911/aoostar-rs.git
cd aoostar-rs
cargo install cargo-generate-rpm --locked
./linux/package.sh rpm
sudo dnf install ./dist-linux/*.rpm
```

Reconnect the LCD, then run `aster-launcher` or launch **AOOSTAR LCD** from
the desktop menu. Remove it with `sudo dnf remove aoostar-rs`.

## Arch Linux

The repository includes a checked PKGBUILD for the current release. Install
the build tools, build it as a regular user, then let `makepkg` install it:

```bash
sudo pacman -S --needed base-devel rust cargo pkgconf systemd dbus
git clone https://github.com/shine911/aoostar-rs.git
cd aoostar-rs/linux
makepkg -si
```

Reconnect the LCD after the install, then run `aster-launcher` or select
**AOOSTAR LCD** from KDE Plasma's application launcher. Remove it with:

```bash
sudo pacman -Rns aoostar-rs
```

## Build and run directly from a Linux checkout

For development, install the distribution's Rust, C compiler, `pkg-config`,
libudev development files, and D-Bus development files. Then run:

```bash
cargo build --release
AOOSTAR_ASSET_DIR="$PWD" AOOSTAR_BIN_DIR="$PWD/target/release" \
  ./target/release/aster-launcher
```

The environment overrides let the launcher use the checkout's `cfg/` and
`fonts/` directories. They are unnecessary after installing a DEB, RPM, or
Arch package.

## Linux configuration and troubleshooting

The launcher creates only per-user state:

- Configuration: `$XDG_CONFIG_HOME/aoostar-rs/launcher.toml`
- Logs: `$XDG_STATE_HOME/aoostar-rs/logs/`
- Runtime files and sensor data: `$XDG_RUNTIME_DIR/aoostar-rs/`

If the LCD cannot be opened after reconnecting it, confirm that the desktop
session has a working `uaccess` seat and inspect the permissions on its
`/dev/tty*` device. If Plasma hides the tray icon, mark **AOOSTAR LCD** as
visible or always shown in System Tray settings.

## Development checks

```bash
cargo fmt --all -- --check
cargo test -p aster-launcher -p aster-sysinfo
```

From WSL, use the repository helper instead of direct Cargo commands:

```bash
./build-from-wsl.sh test
./build-from-wsl.sh linux-packages
```

For panel formats, sensor mappings, and shell display controls, see the
[documentation](docs/README.md) and [Linux shell commands](docs/shell_commands.md).

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
