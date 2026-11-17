# Builds the release exe and the NSIS installer into dist\.
#   powershell -ExecutionPolicy Bypass -File installer\build.ps1
# Set $env:MAKENSIS to point at makensis.exe if it isn't found automatically.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Off Sales POS $version"

$candidates = @(@(
    $env:MAKENSIS,
    (Get-Command makensis -ErrorAction SilentlyContinue).Source,
    "${env:ProgramFiles(x86)}\NSIS\makensis.exe",
    "$env:ProgramFiles\NSIS\makensis.exe",
    "$env:LOCALAPPDATA\tauri\NSIS\makensis.exe"
) | Where-Object { $_ -and (Test-Path $_) })
if (-not $candidates) { throw "makensis.exe not found. Install NSIS (https://nsis.sourceforge.io) or set `$env:MAKENSIS." }
$makensis = $candidates[0]

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

New-Item -ItemType Directory -Force dist | Out-Null
$exe = Join-Path $root "target\release\offsales-pos.exe"
$out = Join-Path $root "dist"
& $makensis /V2 "/DVERSION=$version" "/DEXE=$exe" "/DOUTDIR=$out" installer\offsales-pos.nsi
if ($LASTEXITCODE -ne 0) { throw "makensis failed" }
Write-Host "Built dist\OffSalesPOS-Setup-$version.exe"
