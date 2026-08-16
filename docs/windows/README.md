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

Note for `aster-launcher`: its `requireAdministrator` manifest is linked into every binary built from that crate,
including the unit-test harness, so `cargo test -p aster-launcher` fails with "requires elevation" (os error 740)
in a normal shell. Run it from an elevated shell, or prefix it with `__COMPAT_LAYER=RunAsInvoker` (PowerShell:
`$env:__COMPAT_LAYER = "RunAsInvoker"`) to let the harness start without elevation.

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

> **Prerequisite:** install the official AOOSTAR-X **`PawnIO.exe`** first (download and install it
> per the AOOSTAR-X software requirements). `HwBridge.exe` loads AOOSTAR-X's
> `LibreHardwareMonitorLib.dll`, which reads hardware registers through the PawnIO driver stack;
> without it the temperature / GPU sensors stay empty.

See [sensor-mapping.cfg](https://github.com/shine911/aoostar-rs/blob/main/cfg/sensor-mapping.cfg) for an example
mapping between AOOSTAR-X panel sensor labels and the labels `aster-sysinfo`/`hwbridge` produce on Windows.

## Packaging and running

After building `asterctl`, `aster-sysinfo`, and `aster-launcher` with `cargo build --release`,
and `HwBridge.exe` as described above, assemble a self-contained folder:

```powershell
.\windows\package-dist.ps1
```

This creates `dist\` containing `aster-launcher.exe` and everything it needs: the other 2 Rust
binaries (in `dist\bin\`), `hwbridge\`, `cfg\`, `fonts\`, and a default `launcher.toml`. The
`dist\` folder can be run in place or copied/zipped to another machine. Re-running the script
keeps an existing `dist\launcher.toml` and `dist\logs\`, so your edits and logs survive a rebuild;
quit a running `aster-launcher.exe` first, or the script will stop with an error.

> **Security: put `dist\` somewhere only Administrators can write.**
> `aster-launcher.exe` elevates once and its 3 children inherit that Administrator token, so
> *every* file under `dist\` — the binaries in `bin\`, `hwbridge\*.exe`/`*.dll`, and the paths
> named in `launcher.toml` — effectively runs with full Administrator rights. If `dist\` lives in
> a user-writable location such as `Downloads\`, `Desktop\`, or anywhere under your profile, any
> non-elevated process (or malware running as your user) can replace those files and get a free
> elevation the next time you start the launcher. Install it under `C:\Program Files\` (or another
> directory whose ACL grants write access only to Administrators) and keep it there.

Double-click `dist\aster-launcher.exe` to start `aster-sysinfo`, `asterctl`, and `hwbridge` as
hidden background processes — no console windows, no manually starting 3 separate tools.
Windows will show a single Administrator prompt (hwbridge needs it to read hardware sensors,
and the other 2 inherit the same elevated process so nothing else needs its own prompt). A tray
icon appears once running; right-click it to see status (running / degraded), pick a refresh
interval from the `Refresh time` sub-menu, or choose "Quit" at the bottom of the menu to stop
everything.

Edit `dist\launcher.toml` to change the monitor config file name or the sensor refresh interval, then
restart `aster-launcher.exe` to apply changes. `refresh_time` sets the refresh interval in seconds for
*both* `aster-sysinfo` and `hwbridge`; allowed values are 2, 5, 10, or 30 (default 5). The legacy
per-process keys `sysinfo_refresh` / `hwbridge_refresh` still work when `refresh_time` is not set.

You can also change the refresh interval live from the tray icon: right-click the tray icon and open
the `Refresh time` sub-menu to pick `2s / 5s / 10s / 30s`. The chosen value is written back to
`dist\launcher.toml` (survives restarts) and `aster-sysinfo` + `hwbridge` are restarted automatically
to apply it; the active choice is marked with a check mark.

### LCD display control (on / off / follow screen state)

The tray icon also has a `Display` sub-menu with three mutually exclusive modes (the active one is
marked with a check mark):

- **On** — the LCD stays on (default).
- **Off** — the LCD turns off; `asterctl` keeps the serial port open and simply stops drawing, so
  picking **On** again wakes it instantly with no restart.
- **Follow screen state** — the LCD mirrors the Windows display power state: it turns off when the
  monitor turns off (power button, idle timeout, screensaver, lid close) and back on when the monitor
  turns on. The display state is detected via the `GUID_CONSOLE_DISPLAY_STATE` power-setting
  notification, which also covers idle-blank transitions that never enter system sleep.

The choice is persisted as `display_mode = "on" | "off" | "follow"` in `dist\launcher.toml`
(survives restarts; the key is optional — when absent the LCD stays on). Internally the launcher
writes the mode to `dist\cfg\display.state`, which `asterctl --display-state` polls on every refresh.

**The LCD always turns off when the launcher stops.** The launcher rewrites `display.state` roughly
every 2 seconds as a heartbeat; if `asterctl` sees the file go stale (~10s) — the launcher was
closed, killed in Task Manager, or crashed — it switches the display off and exits to free the
serial port. Quitting from the tray (or `Quit` in the menu) blanks the display the same way before
the child processes are stopped.

Each process's own output goes to `dist\logs\aster-sysinfo.log`, `dist\logs\asterctl.log`, and
`dist\logs\hwbridge.log`; these are truncated at every launcher start, so a log always covers just
the current run. If a process crashes while the launcher is running, it's automatically restarted
and a marker line is appended to its log. The launcher's own problems (e.g. it couldn't create the
tray icon) go to `dist\logs\launcher.log` — it has no console window to print them to.

Only one launcher can run at a time: it holds `dist\.launcher.lock` while running, and a second
instance (an accidental double-click, say) notes that in `launcher.log` and exits without starting
a duplicate set of children.

## Sleep / resume behavior

When Windows goes to sleep, `aster-launcher` detects the power event and
blanks the LCD first (`asterctl` sends CloseTFT 0x0A), waits ~2s for it to
apply, then suspends all three child processes (they are force-stopped; the
watchers do not restart them while the machine is asleep). On wake it waits
~4s for the USB stack, then re-enumerates the AOOSTAR USB UART and restarts
the children with fresh serial handles, so the AOOSTAR display
re-initializes automatically: `asterctl` re-sends the OpenTFT (0x0B)
handshake, which is exactly how the panel is (re)initialized on the wire.

The USB re-enumeration is needed because on Modern Standby (S0) — the only
sleep state these AOOSTAR boards expose — the panel's MCU power-cycles on
wake (you see the AOOSTAR boot animation) while the host keeps a stale USB
link: the device is still enumerated and COM3 opens, but writes fail with
"The semaphore timeout period has expired". The launcher uses a remove +
rescan ladder that tears the stale link down so the panel enumerates fresh.
It deliberately avoids the old Device Manager "Disable → Enable" workaround
(leaves a "restart required" pending state that makes Windows demand a
reboot after repeated cycles) and a plain re-enumerate (looks successful
but does not clear the stale link). If `asterctl` still cannot initialize
the panel, it writes `cfg/uart.stuck` and the launcher escalates — another
re-enumeration, up to 2 rounds — so the recovery is timed by the panel's
actual readiness. Units whose panel recovers from the soft re-init alone
(fresh handle + OpenTFT, no USB disturbance) can set
`restart_uart_on_resume = false` in `launcher.toml`.

Recommended power settings (reduces the chance of a wedged USB port):
- untick "Allow the computer to turn off this device to save power" for the
  AOOSTAR USB device / hub in Device Manager,
- disable USB selective suspend for the current power plan
  (`powercfg /setacvalueindex scheme_current 2a737441-1930-4402-8d77-b2bebba308a3 48e6b7a6-50f5-4782-a5d4-53bb8f07e226 0`
  then `powercfg /setactive scheme_current`).
