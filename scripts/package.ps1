param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    & cargo xtask package $Version --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Rust release tool failed.' }
} finally { Pop-Location }
