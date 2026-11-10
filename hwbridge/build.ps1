<#
.SYNOPSIS
    Fetches HwBridge's pinned NuGet dependencies and builds HwBridge.exe.

.DESCRIPTION
    Downloads each package below from nuget.org, verifies the .nupkg SHA256
    against the pinned value, extracts the one net472 DLL HwBridge needs into
    hwbridge\lib\, then compiles HwBridge.cs with the .NET Framework csc.exe
    that ships with Windows (no .NET SDK needed).

    The set is LibreHardwareMonitorLib 0.9.6 plus its .NET Framework 4.7.2
    runtime dependency closure. Every extracted DLL is byte-identical to the
    one shipped in the official LibreHardwareMonitor v0.9.6 release zip.
    Licenses: see hwbridge\NOTICE.md and hwbridge\licenses\.

    To bump LibreHardwareMonitorLib: update the Version/Sha256 entries below
    (SHA256 of the .nupkg as served by nuget.org), re-run, and compare the
    raw hw_* sensor keys HwBridge writes before and after on real hardware.

.EXAMPLE
    .\hwbridge\build.ps1
#>
$ErrorActionPreference = "Stop"

$Here = $PSScriptRoot
$Lib = Join-Path $Here "lib"
$Cache = Join-Path $Here "packages"

# Id, Version, DLL path inside the .nupkg, SHA256 of the .nupkg.
$Packages = @(
    @{ Id = "LibreHardwareMonitorLib"; Version = "0.9.6"
       Path = "runtimes/win-x64/lib/net472/LibreHardwareMonitorLib.dll"
       Sha256 = "60db5e0d9135f4658b385cae139d3b9eaeac0a8e867d322d20b5dbe4c4039197" },
    @{ Id = "DiskInfoToolkit"; Version = "1.1.2"
       Path = "lib/net472/DiskInfoToolkit.dll"
       Sha256 = "a997f0b3701e20b2ab9296e07ee4f74f877e75015277fb6bd314ade1e2c4db13" },
    @{ Id = "BlackSharp.Core"; Version = "1.0.7"
       Path = "lib/net472/BlackSharp.Core.dll"
       Sha256 = "4f713fe18ee7c85a7c49f28309adaa815714333326aa81fd4d4a81619ecaa0ab" },
    @{ Id = "RAMSPDToolkit-NDD"; Version = "1.4.2"
       Path = "lib/net472/RAMSPDToolkit-NDD.dll"
       Sha256 = "57c893824a68c71538055f242586616d4205a671c300897c21b2a6f88270ce64" },
    @{ Id = "HidSharp"; Version = "2.6.4"
       Path = "lib/net35/HidSharp.dll"
       Sha256 = "cf55d4b4d7b42dc1d8496b7b592ba00d1ecd48a70bc07bd8dabd3e9976d108e7" },
    @{ Id = "System.Memory"; Version = "4.6.3"
       Path = "lib/net462/System.Memory.dll"
       Sha256 = "26078aeb758c9ae985e8bf851f973026061da6a5eb4837204d0c2d2204c72955" },
    @{ Id = "System.Buffers"; Version = "4.6.1"
       Path = "lib/net462/System.Buffers.dll"
       Sha256 = "b00451e91d016fbec091ad1e361f3a7015e1d91d4047f7e48a74455b2a673d79" },
    @{ Id = "System.Numerics.Vectors"; Version = "4.6.1"
       Path = "lib/net462/System.Numerics.Vectors.dll"
       Sha256 = "2bc500a86dcb02f2032d6d877f9e2d6e9e4a79080e57239b4198679d4031f2c7" },
    @{ Id = "System.Runtime.CompilerServices.Unsafe"; Version = "6.1.2"
       Path = "lib/net462/System.Runtime.CompilerServices.Unsafe.dll"
       Sha256 = "5f6a7f53af3465f92beb6da873ebe0e496206c313313b98badee4355a6b25937" },
    @{ Id = "System.Threading.AccessControl"; Version = "10.0.3"
       Path = "lib/net462/System.Threading.AccessControl.dll"
       Sha256 = "9f4f28238e13c6cc47b789a4a1d5647997747594dfcfd45c1526f9b671cde073" },
    @{ Id = "System.Security.AccessControl"; Version = "6.0.0"
       Path = "runtimes/win/lib/net461/System.Security.AccessControl.dll"
       Sha256 = "a8ec961016cdaf7123c92f9eb451bcff331bb8f8c0f8ef9d8bbd7b24ff42c728" },
    @{ Id = "System.Security.Principal.Windows"; Version = "5.0.0"
       Path = "runtimes/win/lib/net461/System.Security.Principal.Windows.dll"
       Sha256 = "081390c25f6f78592b28ada853c24514488a221fe9f9a24efaaf5373643ff3d6" }
)

# Windows PowerShell 5.1 defaults to TLS 1.0, which nuget.org rejects.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression.FileSystem

New-Item -ItemType Directory -Force -Path $Cache | Out-Null
# Start lib\ empty so a dropped dependency never lingers from an older build.
if (Test-Path $Lib) {
    Remove-Item -Recurse -Force $Lib
}
New-Item -ItemType Directory -Force -Path $Lib | Out-Null

foreach ($p in $Packages) {
    $id = $p.Id.ToLowerInvariant()
    $nupkg = Join-Path $Cache ("{0}.{1}.nupkg" -f $id, $p.Version)

    $cached = (Test-Path $nupkg) -and ((Get-FileHash -Algorithm SHA256 $nupkg).Hash -eq $p.Sha256)
    if (-not $cached) {
        $url = "https://api.nuget.org/v3-flatcontainer/$id/$($p.Version)/$id.$($p.Version).nupkg"
        Write-Host "Downloading $($p.Id) $($p.Version)..."
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $nupkg
        $hash = (Get-FileHash -Algorithm SHA256 $nupkg).Hash
        if ($hash -ne $p.Sha256) {
            Remove-Item -Force $nupkg
            throw "SHA256 mismatch for $($p.Id) $($p.Version): expected $($p.Sha256), got $hash"
        }
    }

    $zip = [IO.Compression.ZipFile]::OpenRead($nupkg)
    try {
        $entry = $zip.GetEntry($p.Path)
        if ($null -eq $entry) {
            throw "$($p.Path) not found in $($p.Id) $($p.Version)"
        }
        $dest = Join-Path $Lib ([IO.Path]::GetFileName($p.Path))
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $dest, $true)
    } finally {
        $zip.Dispose()
    }
}

$Csc = Join-Path $env:WINDIR "Microsoft.NET\Framework64\v4.0.30319\csc.exe"
if (-not (Test-Path $Csc)) {
    throw "csc.exe not found at $Csc - install .NET Framework 4.x."
}

Write-Host "Compiling HwBridge.exe..."
& $Csc /nologo /platform:x64 `
    ("/r:" + (Join-Path $Lib "LibreHardwareMonitorLib.dll")) `
    ("/out:" + (Join-Path $Here "HwBridge.exe")) `
    (Join-Path $Here "HwBridge.cs")
if ($LASTEXITCODE -ne 0) {
    throw "csc.exe failed with exit code $LASTEXITCODE"
}

Write-Host "HwBridge.exe built; runtime DLLs are in hwbridge\lib\."
