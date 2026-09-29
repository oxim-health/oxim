<#
.SYNOPSIS
    Builds the offline installation bundle for a Windows target.

.DESCRIPTION
    Writes <OutDir>\oxim-<version>-<target>.zip and a .sha256 file next to
    it. The bundle holds oxim.exe, the configuration template, example
    channels and tables, the installation guides, the licenses,
    install.ps1 / uninstall.ps1 and SHA256SUMS of its own content.
    Installing it needs no network access. Linux and macOS bundles are
    built by build-bundle.sh. Works with Windows PowerShell 5.1 and
    PowerShell 7.

.EXAMPLE
    .\deploy\offline\build-bundle.ps1 -Version 1.0.0 `
        -Target x86_64-pc-windows-msvc `
        -Binary target\x86_64-pc-windows-msvc\release\oxim.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Version,
    [Parameter(Mandatory = $true)]
    [string] $Target,
    [Parameter(Mandatory = $true)]
    [string] $Binary,
    [string] $OutDir = 'dist'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Binary = (Resolve-Path $Binary).Path
$Version = $Version -replace '^v', ''
$name = "oxim-$Version-$Target"

$stage = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
$root = Join-Path $stage $name
try {
    foreach ($dir in @('config', 'examples\channels', 'examples\tables', 'docs')) {
        New-Item -ItemType Directory -Force -Path (Join-Path $root $dir) | Out-Null
    }
    Copy-Item $Binary (Join-Path $root 'oxim.exe')
    Copy-Item (Join-Path $repo 'deploy\windows\oxim.yaml') (Join-Path $root 'config\oxim.yaml')
    Copy-Item (Join-Path $repo 'deploy\examples\channels\*') (Join-Path $root 'examples\channels')
    Copy-Item (Join-Path $repo 'deploy\examples\tables\*') (Join-Path $root 'examples\tables')
    Copy-Item (Join-Path $repo 'docs\install\*.md') (Join-Path $root 'docs')
    foreach ($file in @('README.md', 'LICENSE-MIT', 'LICENSE-APACHE')) {
        Copy-Item (Join-Path $repo $file) $root
    }
    foreach ($file in @('install.ps1', 'uninstall.ps1')) {
        Copy-Item (Join-Path $PSScriptRoot $file) $root
    }
    [System.IO.File]::WriteAllText((Join-Path $root 'VERSION'), "$Version`n")

    # SHA256SUMS in the format of sha256sum: "<hash>  <relative path>".
    $lines = Get-ChildItem -Path $root -Recurse -File |
        Sort-Object FullName |
        ForEach-Object {
            $relative = $_.FullName.Substring($root.Length + 1).Replace('\', '/')
            $hash = (Get-FileHash -Algorithm SHA256 -Path $_.FullName).Hash.ToLowerInvariant()
            "$hash  $relative"
        }
    [System.IO.File]::WriteAllText((Join-Path $root 'SHA256SUMS'), (($lines -join "`n") + "`n"))

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    $archive = Join-Path (Resolve-Path $OutDir).Path "$name.zip"
    if (Test-Path $archive) { Remove-Item $archive }
    Compress-Archive -Path $root -DestinationPath $archive
    $hash = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText("$archive.sha256", "$hash  $name.zip`n")
    Write-Host "created $archive"
}
finally {
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
}
