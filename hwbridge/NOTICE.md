# Third-Party Binaries Notice

The `.dll` files in this folder are **not** part of this project. They are copied as-is from the
official AOOSTAR-X installation directory (the vendor software this project reverse-engineers), so
that `HwBridge.cs` can load the same hardware-sensor stack AOOSTAR-X itself uses.

| File | Origin |
|---|---|
| `LibreHardwareMonitorLib.dll` | [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor), bundled by AOOSTAR-X |
| `DiskInfoToolkit.dll`, `RAMSPDToolkit-NDD.dll`, `Aga.Controls.dll`, `BlackSharp.Core.dll`, `OxyPlot.dll`, `OxyPlot.WindowsForms.dll`, `HidSharp.dll`, `Microsoft.Win32.TaskScheduler.dll`, `inpoutx64.dll`, `libryzenadj.dll` | Bundled dependencies of the AOOSTAR-X installation |
| Remaining `System.*.dll` / `Microsoft.Bcl.*.dll` | .NET BCL polyfill packages, bundled by AOOSTAR-X |

They are included here only to make `HwBridge.cs` buildable without hunting down each dependency
separately. No claim of authorship or license is made over these files — all rights remain with
their respective owners (AOOSTAR-X / LibreHardwareMonitor / their upstream authors). Use, redistribute,
or remove them at your own judgment and risk; this project takes no responsibility for them.
