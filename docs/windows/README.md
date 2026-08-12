# Windows

## Build from source

### asterctl / aster-sysinfo

1. Install Rust and Cargo, following the [Rust installation page](https://www.rust-lang.org/tools/install).
   At least Rust version 1.88 is required.
2. Checkout project:

```shell
git clone https://github.com/shine911/aoostar-rs.git
cd aoostar-rs
```

3. Build:

```shell
cargo build --release
```

The binaries will be located in `.\target\release\`.

Unlike Linux, no extra build dependencies (`libudev-dev` etc.) are required — the serial port and sysinfo crates
use native Windows APIs.

### hwbridge

`aster-sysinfo` can only read the sensors exposed by the [sysinfo](https://github.com/GuillaumeGomez/sysinfo) crate.
On Windows, this does not include per-component temperatures, GPU load, or motherboard/memory sensors.
[hwbridge](https://github.com/shine911/aoostar-rs/blob/main/hwbridge/HwBridge.cs) is a small standalone tool that
loads the same `LibreHardwareMonitorLib.dll` AOOSTAR-X itself uses, and writes those additional sensor values into
a text file that `asterctl` picks up like any other sensor source (see
[Text File Data Source](../sensor/provider/text_file.md)).

It is written in deliberately old-style C# (no tuples, no string interpolation, no LINQ), so it compiles with the
plain .NET Framework compiler that ships with Windows — no .NET SDK install required.

1. Locate the .NET Framework C# compiler, typically:

```shell
C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe
```

2. Build `HwBridge.exe`, referencing the vendored DLLs in the `hwbridge` folder:

```shell
cd hwbridge
C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe /nologo /r:LibreHardwareMonitorLib.dll /out:HwBridge.exe HwBridge.cs
```

3. Run `HwBridge.exe` **as Administrator** — required because the underlying `LibreHardwareMonitorLib`/PawnIO stack
   needs elevated access to read hardware registers, the same requirement AOOSTAR-X itself has.

See [sensor-mapping.cfg](https://github.com/shine911/aoostar-rs/blob/main/cfg/sensor-mapping.cfg) for an example
mapping between AOOSTAR-X panel sensor labels and the labels `aster-sysinfo`/`hwbridge` produce on Windows.

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
