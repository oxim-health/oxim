<#
.SYNOPSIS
    Removes OXIM installed by install.ps1.

.DESCRIPTION
    Stops and removes the "oxim" service, deletes the program directory and
    its PATH entry. The configuration, channels and stored messages in
    %ProgramData%\OXIM are kept unless -Purge is given.

        powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 [-Purge]
#>
[CmdletBinding()]
param(
    [string] $InstallDir = (Join-Path $env:ProgramFiles 'OXIM'),
    [string] $DataDir = (Join-Path $env:ProgramData 'OXIM'),
    [switch] $Purge
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script from an elevated PowerShell (Run as administrator).'
}

$oxim = Join-Path $InstallDir 'oxim.exe'
if (Get-Service -Name 'oxim' -ErrorAction SilentlyContinue) {
    if (Test-Path $oxim) {
        & $oxim service uninstall
        if ($LASTEXITCODE -ne 0) { throw 'oxim service uninstall failed' }
    } else {
        Stop-Service -Name 'oxim' -ErrorAction SilentlyContinue
        & sc.exe delete oxim | Out-Null
    }
}

$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$remaining = ($machinePath -split ';') | Where-Object { $_ -and $_ -ne $InstallDir }
[Environment]::SetEnvironmentVariable('Path', ($remaining -join ';'), 'Machine')

# This script may run from the program directory; remove it last.
if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }

if ($Purge) {
    if (Test-Path $DataDir) { Remove-Item -Recurse -Force $DataDir }
    Write-Host 'OXIM, its configuration and its data are removed.'
} else {
    Write-Host "OXIM is removed. Kept $DataDir (use -Purge to delete it)."
}
