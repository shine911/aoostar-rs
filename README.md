# AOOSTAR WTR MAX / GEM12+ PRO Screen Control

Reverse engineering the [AOOSTAR WTR MAX](https://aoostar.com/products/aoostar-wtr-max-amd-r7-pro-8845hs-11-bays-mini-pc)
display protocol, with a proof-of-concept application written in Rust.  
It has only been tested on the WTR MAX, but should also support the GEM12+ PRO device.

Check out the **[User Guide](https://shine911.github.io/aoostar-rs)** for a list of features and installation and usage information.

## Features

- Control the AOOSTAR WTR MAX and GEM12+ PRO second screen from Linux or Windows.
- Switch the display on or off.
    - Also possible with standard [Linux shell commands](docs/shell_commands.md).
    - [Linux systemd Service](docs/linux/README.md) to automatically switch off the LCD at boot up.
- Display images (with automatic scaling and partial update support).
- Render dynamic sensor panels defined from the AOOSTAR-X software.
    - Update sensor values from simple text files and/or the `AOOSTAR_HW_STATS` shared memory region.
    - Rotate through multiple panels in a defined interval.
    - On Windows, [hwbridge](hwbridge/HwBridge.cs) supplements `aster-sysinfo` with CPU/GPU/motherboard/memory
      temperatures and GPU load (via `LibreHardwareMonitorLib.dll`), data aster-sysinfo cannot read on Windows alone.
      In `--shm` mode (used by the launcher) `HwBridge` and `aster-sysinfo` publish sensor values into the
      `AOOSTAR_HW_STATS` shared memory region, which `asterctl --shm` reads directly — no file I/O in the hot path.
- USB device/serial port selection.

## Disclaimer

> I take no responsibility for the use of this software.  
> There is no official documentation available;
> all display control commands have been reverse engineered from the original AOOSTAR-X software.

Even though this software works fine **for me**, I cannot guarantee that it is risk-free:

- It may or may not work.
- It could crash the display firmware, requiring a power cycle.
- It could even brick the display firmware.
- You have been warned!

The risk remains until the manufacturer provides official documentation, and the protocol can be reviewed.
Note: Multiple attempts to contact the manufacturer for documentation have received no response.

With that out of the way, on to the fun stuff!

- Browse the source code or read the [User Guide](https://shine911.github.io/aoostar-rs)
- See [releases](https://github.com/shine911/aoostar-rs/releases) for binary Linux x64 releases. A Debian package for easy installation is planned for the future!

## Contributing

Pull requests are welcome. For major changes, please open an issue first to discuss what you would like to change.

Please note that this software is currently in its initial development and will have major changes until the mentioned
goals above are reached!

## Credits

This is a fork of [zehnm/aoostar-rs](https://github.com/zehnm/aoostar-rs), the original project by
[Markus Zehnder](https://github.com/zehnm) reverse-engineering the AOOSTAR display protocol. All credit for the
original work goes to the upstream project; this fork adds Windows support on top of it.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
