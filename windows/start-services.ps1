<#
.SYNOPSIS
    Starts aster-sysinfo, asterctl, and hwbridge in separate windows.

.DESCRIPTION
    Replaces manually opening 3 terminal tabs and running each tool by hand.
    Run this after `cargo build --release` and after building hwbridge (see
    docs/windows/README.md).

    hwbridge requires Administrator privileges to read hardware sensors, so
    its window is launched elevated (a UAC prompt will appear for it only).

.PARAMETER MonitorConfig
    AOOSTAR-X panel config file passed to asterctl's --config option.

.PARAMETER SysinfoRefresh
    aster-sysinfo --refresh interval in seconds.

.PARAMETER HwBridgeRefresh
    hwbridge refresh interval in seconds.

.EXAMPLE
    .\windows\start-services.ps1
    .\windows\start-services.ps1 -MonitorConfig Monitor3.json -SysinfoRefresh 2 -HwBridgeRefresh 5
#>
param(
    [string]$MonitorConfig = "Monitor3.json",
    [int]$SysinfoRefresh = 2,
    [int]$HwBridgeRefresh = 5
)

$ErrorActionPreference = "Stop"

# Repo root is the parent of this script's directory (windows\start-services.ps1 -> repo root)
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$AsterSysinfo = Join-Path $RepoRoot "target\release\aster-sysinfo.exe"
$AsterCtl = Join-Path $RepoRoot "target\release\asterctl.exe"
$HwBridge = Join-Path $RepoRoot "hwbridge\HwBridge.exe"
$SensorsDir = Join-Path $RepoRoot "cfg\sensors"

foreach ($exe in @($AsterSysinfo, $AsterCtl, $HwBridge)) {
    if (-not (Test-Path $exe)) {
        throw "Missing $exe - build it first (see docs/windows/README.md)."
    }
}

New-Item -ItemType Directory -Force -Path $SensorsDir | Out-Null

Write-Host "Starting aster-sysinfo..."
Start-Process powershell.exe -WorkingDirectory $RepoRoot -ArgumentList @(
    "-NoExit", "-Command",
    "$AsterSysinfo --out cfg\sensors\sysinfo.txt --temp-dir cfg\sensors --refresh $SysinfoRefresh"
)

Write-Host "Starting asterctl..."
Start-Process powershell.exe -WorkingDirectory $RepoRoot -ArgumentList @(
    "-NoExit", "-Command",
    "$AsterCtl --config $MonitorConfig"
)

Write-Host "Starting hwbridge (elevated, requires Administrator)..."
Start-Process powershell.exe -WorkingDirectory $RepoRoot -Verb RunAs -ArgumentList @(
    "-NoExit", "-Command",
    "$HwBridge cfg\sensors\hwbridge.txt $HwBridgeRefresh"
)

Write-Host "All 3 services launched in separate windows. Close each window to stop it."
