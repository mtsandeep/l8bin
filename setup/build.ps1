# build.ps1
# Automates local build and prepares 'release' folder for testing the installer.
# This script should be located in the /setup folder.

$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$OS = "windows"

# Find repository root (one level up from /setup)
$RootDir = Split-Path $PSScriptRoot -Parent
Push-Location $RootDir

Write-Host "Building LiteBin components in release mode..." -ForegroundColor Cyan
cargo build --release

if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed!" -ForegroundColor Red
    Pop-Location
    exit 1
}

$ReleaseDir = Join-Path $RootDir "release"
if (-not (Test-Path $ReleaseDir)) {
    New-Item -ItemType Directory -Path $ReleaseDir | Out-Null
}

# Map of internal names to desired release names
$Binaries = @{
    "l8b" = "l8b"
    "litebin-agent" = "litebin-agent"
    "litebin-orchestrator" = "litebin-orchestrator"
}

Write-Host "`nPreparing 'release' folder for installer..." -ForegroundColor Cyan

foreach ($bin in $Binaries.Keys) {
    $src = Join-Path $RootDir "target\release\$($Binaries[$bin]).exe"
    $dest = Join-Path $ReleaseDir "$bin-$Arch-$OS.exe"
    
    if (Test-Path $src) {
        Copy-Item $src $dest -Force
        Write-Host "  [OK] $src -> $dest" -ForegroundColor Green
    } else {
        Write-Host "  [SKIP] $src not found." -ForegroundColor DarkGray
    }
}

Write-Host "`nDone! You can now test the installer locally:" -ForegroundColor White
Write-Host "powershell -ExecutionPolicy ByPass -File .\install-windows.ps1 cli" -ForegroundColor Green

Pop-Location
