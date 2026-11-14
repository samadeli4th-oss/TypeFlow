# Builds the release app, then the installer that carries it:
#   dist\TypeFlow-Setup-<version>.exe
#
#   powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1        # MSVC toolchain
#   powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1 -Gnu   # GNU toolchain
#
# -Gnu builds with the stable-x86_64-pc-windows-gnu toolchain, which needs no
# Visual Studio (rustup toolchain install stable-x86_64-pc-windows-gnu).
param([switch]$Gnu)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$cargo = @('build', '--release')
$target = Join-Path $root 'target'
if ($Gnu) {
    $cargo = @('+stable-x86_64-pc-windows-gnu') + $cargo
    $target = Join-Path $target 'gnu'
}

Push-Location $root
try {
    & cargo @cargo --target-dir $target
    if ($LASTEXITCODE -ne 0) { throw 'building TypeFlow failed' }
    $env:TYPEFLOW_EXE = Join-Path $target 'release\typeflow.exe'

    Push-Location (Join-Path $root 'installer')
    try {
        & cargo @cargo
        if ($LASTEXITCODE -ne 0) { throw 'building the installer failed' }
    } finally { Pop-Location }

    $version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    $out = Join-Path $dist "TypeFlow-Setup-$version.exe"
    Copy-Item (Join-Path $root 'installer\target\release\typeflow-setup.exe') $out -Force
    Write-Host "Installer: $out"
} finally {
    Remove-Item Env:TYPEFLOW_EXE -ErrorAction SilentlyContinue
    Pop-Location
}
