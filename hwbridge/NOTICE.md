# Third-Party Components

`HwBridge.exe` uses the open-source libraries below. They are not stored in this repository:
`build.ps1` downloads each pinned package from [nuget.org](https://www.nuget.org/), checks its SHA256,
and extracts the DLL into `lib\`. These DLLs are byte-identical to the ones shipped in the official
[LibreHardwareMonitor v0.9.6](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/tag/v0.9.6)
release.

| Component | Version | License | Source |
|---|---|---|---|
| LibreHardwareMonitorLib | 0.9.6 | MPL-2.0 | <https://github.com/LibreHardwareMonitor/LibreHardwareMonitor> |
| DiskInfoToolkit | 1.1.2 | MPL-2.0 | <https://github.com/Blacktempel/DiskInfoToolkit> |
| BlackSharp.Core | 1.0.7 | MPL-2.0 | <https://github.com/Blacktempel/BlackSharp> |
| RAMSPDToolkit-NDD | 1.4.2 | MPL-2.0 | <https://github.com/Blacktempel/RAMSPDToolkit> |
| HidSharp | 2.6.4 | Apache-2.0 | <https://software.seekye.com/hidsharp> |
| System.Memory | 4.6.3 | MIT | <https://github.com/dotnet/maintenance-packages> |
| System.Buffers | 4.6.1 | MIT | <https://github.com/dotnet/maintenance-packages> |
| System.Numerics.Vectors | 4.6.1 | MIT | <https://github.com/dotnet/maintenance-packages> |
| System.Runtime.CompilerServices.Unsafe | 6.1.2 | MIT | <https://github.com/dotnet/maintenance-packages> |
| System.Threading.AccessControl | 10.0.3 | MIT | <https://github.com/dotnet/runtime> |
| System.Security.AccessControl | 6.0.0 | MIT | <https://github.com/dotnet/runtime> |
| System.Security.Principal.Windows | 5.0.0 | MIT | <https://github.com/dotnet/runtime> |

The MPL-2.0 components are distributed unmodified; their source code is available at the
repositories above. License texts are in `licenses\`:

- `MPL-2.0.txt` — Mozilla Public License 2.0
- `HidSharp-Apache-2.0.txt` — HidSharp's Apache License 2.0 notice
- `MIT-dotnet.txt` — MIT License, .NET Foundation and Contributors

Hardware access goes through the [PawnIO](https://pawnio.eu/) driver, which is not bundled and must be
installed separately.
