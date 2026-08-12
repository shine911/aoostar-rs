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

if (Test-Path $Dist) {
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

Write-Host "Copying default launcher.toml..."
Copy-Item (Join-Path $RepoRoot "windows\launcher.default.toml") (Join-Path $Dist "launcher.toml")

Write-Host "dist\ ready. Double-click dist\aster-launcher.exe to run."
