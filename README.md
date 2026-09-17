# AOOSTAR LCD setup guide

[![Git](https://img.shields.io/github/last-commit/shine911/aoostar-rs?label=git)](https://github.com/shine911/aoostar-rs/commits)
[![Build](https://github.com/shine911/aoostar-rs/actions/workflows/build.yml/badge.svg)](https://github.com/shine911/aoostar-rs/actions/workflows/build.yml)
[![Version](https://img.shields.io/github/v/release/shine911/aoostar-rs?label=version)](https://github.com/shine911/aoostar-rs/releases)

This project controls the secondary LCD in the AOOSTAR WTR MAX and GEM12+ PRO.
It is a fork of the original [zehnm/aoostar-rs](https://github.com/zehnm/aoostar-rs).

> **Hardware warning:** this protocol is reverse engineered. Use it at your own
> risk; a failed display update may require a power cycle.

## Choose an installation method

| Platform | Download or install | Start command |
| --- | --- | --- |
| Windows 10/11 x64 | `*-windows-x64.zip` from Releases | `aster-launcher.exe` |
| Debian/Ubuntu | `.deb` from Releases | `aster-launcher` |
| Fedora/RHEL-family | `.rpm` from Releases | `aster-launcher` |
| Arch Linux | Build `linux/PKGBUILD` | `aster-launcher` |

On Linux, the launcher is a KDE Plasma 5/6 tray application. It works on X11
and Wayland and does not install an autostart service.

## Windows

1. Download the latest `aoostar-rs-*-windows-x64.zip` from
   [Releases](https://github.com/shine911/aoostar-rs/releases) and extract it.
2. Move the extracted files to an Administrator-writable directory such as
   `C:\Program Files\AOOSTAR LCD`. Do not run it from a user-writable folder:
   the launcher and its children run elevated.
3. Download and install [PawnIO](https://pawnio.eu/) before starting if you
   need CPU/GPU/motherboard temperatures and GPU load. Basic LCD control and
   system sensors work without it.
4. Start `aster-launcher.exe` and approve the Administrator prompt. Use its
   tray icon to set refresh time, theme, display state, or quit.

Edit `launcher.toml` to select a panel or set `refresh_time` to `2`, `5`,
`10`, or `30` seconds. Logs are in `logs\`.

<details>
<summary>Build the Windows distribution from source</summary>

Install Rust 1.88 or newer with the MSVC toolchain, Visual Studio Build Tools
with **Desktop development with C++**, and .NET Framework 4.x. Then run
PowerShell from the repository root:

```powershell
git clone https://github.com/shine911/aoostar-rs.git
cd aoostar-rs
cargo build --release

cd hwbridge
& "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo /r:LibreHardwareMonitorLib.dll /out:HwBridge.exe HwBridge.cs
cd ..

.\windows\package-dist.ps1
```

The complete portable installation is written to `dist\`.
</details>

## Debian and Ubuntu

1. Download the latest `.deb` from
   [Releases](https://github.com/shine911/aoostar-rs/releases).
2. Install it:

   ```bash
   sudo apt install ./aoostar-rs-*.deb
   ```

3. Open **AOOSTAR LCD** from the application launcher, or run
   `aster-launcher`.

Remove it with `sudo apt remove aoostar-rs`.

<details>
<summary>Build the DEB from source</summary>

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
</details>

## Fedora and related distributions

1. Download the latest `.rpm` from
   [Releases](https://github.com/shine911/aoostar-rs/releases).
2. Install it:

   ```bash
   sudo dnf install ./aoostar-rs-*.rpm
   ```

3. Open **AOOSTAR LCD** from the application launcher, or run
   `aster-launcher`.

Remove it with `sudo dnf remove aoostar-rs`.

<details>
<summary>Build the RPM from source</summary>

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
</details>

## Arch Linux

No official Arch binary package is published. After installing with the
PKGBUILD below, open **AOOSTAR LCD** from KDE Plasma's application launcher
or run `aster-launcher`.

Remove it with `sudo pacman -Rns aoostar-rs`.

<details>
<summary>Build and install the Arch package</summary>

Build as a regular user:

```bash
sudo pacman -S --needed base-devel rust cargo pkgconf systemd dbus
git clone https://github.com/shine911/aoostar-rs.git
cd aoostar-rs/linux
makepkg -si
```
</details>

## Linux configuration and troubleshooting

The launcher creates only per-user state:

- Configuration: `$XDG_CONFIG_HOME/aoostar-rs/launcher.toml`
- Logs: `$XDG_STATE_HOME/aoostar-rs/logs/`
- Runtime files and sensor data: `$XDG_RUNTIME_DIR/aoostar-rs/`

If the LCD cannot be opened, confirm that the desktop session has a working
`uaccess` seat and inspect the permissions on its `/dev/tty*` device. If Plasma
hides the tray icon, mark **AOOSTAR LCD** as visible or always shown in System
Tray settings.

<details>
<summary>Development builds and checks</summary>

For a Linux checkout, install the distribution's Rust, C compiler,
`pkg-config`, libudev development files, and D-Bus development files. Then:

```bash
cargo build --release
AOOSTAR_ASSET_DIR="$PWD" AOOSTAR_BIN_DIR="$PWD/target/release" \
  ./target/release/aster-launcher

cargo fmt --all -- --check
cargo test -p aster-launcher -p aster-sysinfo
```

From WSL, use the repository helper:

```bash
./build-from-wsl.sh test
./build-from-wsl.sh linux-packages
```
</details>

For panel formats, sensor mappings, and shell display controls, see the
[documentation](docs/README.md) and [Linux shell commands](docs/shell_commands.md).

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
