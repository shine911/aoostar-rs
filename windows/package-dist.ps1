<#
.SYNOPSIS
    Assembles a self-contained dist\ folder around aster-launcher.exe.

.DESCRIPTION
    Run after `cargo build --release` (builds aster-launcher, aster-sysinfo,
    asterctl) and after building hwbridge\HwBridge.exe (see
    docs/windows/README.md). Copies everything aster-launcher needs at
    runtime into dist\, so that folder can be run in place or zipped up and
    handed to another machine.

.EXAMPLE
    .\windows\package-dist.ps1
#>
$ErrorActionPreference = "Stop"

# Repo root is the parent of this script's directory (windows\package-dist.ps1 -> repo root)
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$ReleaseDir = Join-Path $RepoRoot "target\release"
$Dist = Join-Path $RepoRoot "dist"

$RequiredFiles = @(
    (Join-Path $ReleaseDir "aster-launcher.exe"),
    (Join-Path $ReleaseDir "aster-sysinfo.exe"),
    (Join-Path $ReleaseDir "asterctl.exe"),
    (Join-Path $RepoRoot "hwbridge\HwBridge.exe")
)
foreach ($file in $RequiredFiles) {
    if (-not (Test-Path $file)) {
        throw "Missing $file - build it first (see docs/windows/README.md)."
    }
}

$LauncherExe = Join-Path $Dist "aster-launcher.exe"
$PreserveDir = $null
$PreservedToml = $null
$PreservedLogs = $null

if (Test-Path $Dist) {
    # A running aster-launcher.exe holds a lock on its own image, so the
    # Remove-Item below would fail partway through and leave a half-deleted
    # dist\ behind. Probe that one file first so the user gets a clear
    # message instead of a generic "file in use" exception mid-copy.
    if (Test-Path $LauncherExe) {
        try {
            Remove-Item -Force $LauncherExe -ErrorAction Stop
        } catch {
            throw ("Cannot delete $LauncherExe - aster-launcher.exe appears to be running. " +
                   "Quit it first (right-click its tray icon -> Quit All), then re-run this script.")
        }
    }

    # Preserve user-owned state across a rebuild: an edited launcher.toml and
    # the accumulated logs must survive re-packaging.
    $PreserveDir = Join-Path ([System.IO.Path]::GetTempPath()) ("aster-dist-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $PreserveDir | Out-Null
    if (Test-Path (Join-Path $Dist "launcher.toml")) {
        Copy-Item (Join-Path $Dist "launcher.toml") $PreserveDir
        $PreservedToml = Join-Path $PreserveDir "launcher.toml"
    }
    if (Test-Path (Join-Path $Dist "logs")) {
        Copy-Item (Join-Path $Dist "logs") (Join-Path $PreserveDir "logs") -Recurse
        $PreservedLogs = Join-Path $PreserveDir "logs"
    }

    Remove-Item -Recurse -Force $Dist
}
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $Dist "bin") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $Dist "hwbridge") | Out-Null

Write-Host "Copying binaries..."
Copy-Item (Join-Path $ReleaseDir "aster-launcher.exe") $Dist
Copy-Item (Join-Path $ReleaseDir "aster-sysinfo.exe") (Join-Path $Dist "bin")
Copy-Item (Join-Path $ReleaseDir "asterctl.exe") (Join-Path $Dist "bin")

Write-Host "Copying hwbridge..."
Copy-Item (Join-Path $RepoRoot "hwbridge\*.exe") (Join-Path $Dist "hwbridge")
Copy-Item (Join-Path $RepoRoot "hwbridge\*.dll") (Join-Path $Dist "hwbridge")

Write-Host "Copying cfg..."
Copy-Item (Join-Path $RepoRoot "cfg") (Join-Path $Dist "cfg") -Recurse

# asterctl watches cfg\sensors for sensor-file changes and exits if the
# directory does not exist yet when it starts watching. Created after the cfg
# copy above (Copy-Item -Recurse would nest a second cfg\ inside an existing
# destination directory) so asterctl never burns a crash+restart on first run.
New-Item -ItemType Directory -Force -Path (Join-Path $Dist "cfg\sensors") | Out-Null

# asterctl resolves its default --font-dir ("fonts") relative to its working
# directory, which the launcher sets to dist\. Without this copy it silently
# falls back to a compiled-in font with no CJK coverage.
Write-Host "Copying fonts..."
Copy-Item (Join-Path $RepoRoot "fonts") (Join-Path $Dist "fonts") -Recurse

if ($PreservedToml) {
    Write-Host "Keeping existing launcher.toml..."
    Copy-Item $PreservedToml (Join-Path $Dist "launcher.toml")
} else {
    Write-Host "Copying default launcher.toml..."
    Copy-Item (Join-Path $RepoRoot "windows\launcher.default.toml") (Join-Path $Dist "launcher.toml")
}

if ($PreservedLogs) {
    Write-Host "Keeping existing logs..."
    Copy-Item $PreservedLogs (Join-Path $Dist "logs") -Recurse
}

if ($PreserveDir) {
    Remove-Item -Recurse -Force $PreserveDir
}

Write-Host "dist\ ready. Double-click dist\aster-launcher.exe to run."
