param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    & cargo xtask set-version $Version
    if ($LASTEXITCODE -ne 0) { throw 'Rust release tool failed.' }
} finally { Pop-Location }
