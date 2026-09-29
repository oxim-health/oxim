<#
.SYNOPSIS
    Installs or upgrades OXIM from this offline bundle on Windows.

.DESCRIPTION
    Needs no network access. Run from an elevated PowerShell:

        powershell -ExecutionPolicy Bypass -File .\install.ps1 [-Start]

    Installs oxim.exe to %ProgramFiles%\OXIM (added to the system PATH),
    creates %ProgramData%\OXIM with the configuration, channel, table, data
    and log directories (readable only by SYSTEM and Administrators) and
    registers the "oxim" Windows service with `oxim service install`
    (automatic start). An existing oxim.yaml and all channel files are kept.
    Do not mix this with the MSI installer on the same machine.

.PARAMETER Start
    Start the service after installing it.
#>
[CmdletBinding()]
param(
    [string] $InstallDir = (Join-Path $env:ProgramFiles 'OXIM'),
    [string] $DataDir = (Join-Path $env:ProgramData 'OXIM'),
    [switch] $Start
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script from an elevated PowerShell (Run as administrator).'
}

$here = $PSScriptRoot
Write-Host 'Verifying the bundle'
foreach ($line in [System.IO.File]::ReadAllLines((Join-Path $here 'SHA256SUMS'))) {
    if (-not $line) { continue }
    $expected, $relative = $line -split '  ', 2
    $path = Join-Path $here ($relative.Replace('/', '\'))
    $actual = (Get-FileHash -Algorithm SHA256 -Path $path).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "checksum mismatch: $relative" }
}

$service = Get-Service -Name 'oxim' -ErrorAction SilentlyContinue
$wasRunning = $service -and $service.Status -eq 'Running'
if ($wasRunning) {
    Write-Host 'Stopping the running service'
    Stop-Service -Name 'oxim'
    (Get-Service -Name 'oxim').WaitForStatus('Stopped', (New-TimeSpan -Seconds 90))
}

Write-Host "Installing to $InstallDir"
foreach ($dir in @($InstallDir, (Join-Path $InstallDir 'examples'), (Join-Path $InstallDir 'docs'))) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}
Copy-Item (Join-Path $here 'oxim.exe') $InstallDir -Force
foreach ($file in @('README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'VERSION', 'uninstall.ps1')) {
    Copy-Item (Join-Path $here $file) $InstallDir -Force
}
Copy-Item (Join-Path $here 'examples\*') (Join-Path $InstallDir 'examples') -Recurse -Force
Copy-Item (Join-Path $here 'docs\*') (Join-Path $InstallDir 'docs') -Recurse -Force

Write-Host "Preparing $DataDir"
foreach ($dir in @($DataDir, 'data', 'channels', 'tables', 'logs')) {
    $path = if ($dir -eq $DataDir) { $DataDir } else { Join-Path $DataDir $dir }
    New-Item -ItemType Directory -Force -Path $path | Out-Null
}
# Clinical messages and executable channel files: SYSTEM and Administrators only.
& icacls.exe $DataDir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' /T /Q | Out-Null
if ($LASTEXITCODE -ne 0) { throw "icacls failed for $DataDir" }

$config = Join-Path $DataDir 'oxim.yaml'
if (Test-Path $config) {
    Write-Host "Keeping $config"
} else {
    Copy-Item (Join-Path $here 'config\oxim.yaml') $config
}

$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if (($machinePath -split ';') -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', ($machinePath.TrimEnd(';') + ';' + $InstallDir), 'Machine')
}

$oxim = Join-Path $InstallDir 'oxim.exe'
if (-not $service) {
    Write-Host 'Registering the service'
    & $oxim -c $config service install
    if ($LASTEXITCODE -ne 0) { throw 'oxim service install failed' }
}
if ($Start -or $wasRunning) {
    & $oxim service start
    if ($LASTEXITCODE -ne 0) { throw 'oxim service start failed' }
}

Write-Host ''
Write-Host "OXIM $((Get-Content (Join-Path $here 'VERSION')).Trim()) is installed."
Write-Host "Add channel files to $DataDir\channels (examples: $InstallDir\examples),"
Write-Host "check them with 'oxim -c $config validate' and start the service with"
Write-Host "'oxim service start'."
