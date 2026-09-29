<#
.SYNOPSIS
    Builds the OXIM Windows installer (MSI) with the WiX Toolset.

.DESCRIPTION
    Generates the license text shown by the installer from LICENSE-MIT and
    LICENSE-APACHE, then runs `wix build` on deploy/windows/oxim.wxs.
    Requires the WiX Toolset v5 .NET tool (`dotnet tool install --global
    wix --version 5.0.2`); the UI extension is added on first use.
    Works with Windows PowerShell 5.1 and PowerShell 7.

.PARAMETER Version
    Package version, for example 1.0.0. A pre-release suffix such as
    "-rc.1" is dropped because MSI versions are numeric.

.PARAMETER BinDir
    Directory that contains oxim.exe. Defaults to target\release.

.PARAMETER Arch
    x64 or arm64.

.PARAMETER OutDir
    Where the MSI is written. Defaults to dist.

.EXAMPLE
    cargo build --release --locked -p oxim
    .\deploy\windows\build-msi.ps1 -Version 1.0.0
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Version,
    [string] $BinDir,
    [ValidateSet('x64', 'arm64')]
    [string] $Arch = 'x64',
    [string] $OutDir,
    [string] $UiExtension = 'WixToolset.UI.wixext/5.0.2'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $BinDir) { $BinDir = Join-Path $repo 'target\release' }
if (-not $OutDir) { $OutDir = Join-Path $repo 'dist' }
$BinDir = (Resolve-Path $BinDir).Path
if (-not (Test-Path (Join-Path $BinDir 'oxim.exe'))) {
    throw "oxim.exe not found in $BinDir; build it with: cargo build --release --locked -p oxim"
}
if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw 'wix not found; install it with: dotnet tool install --global wix --version 5.0.2'
}

$numeric = ($Version -replace '^v', '') -replace '[-+].*$', ''
if ($numeric -notmatch '^\d+\.\d+\.\d+$') {
    throw "version '$Version' is not of the form major.minor.patch"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$work = Join-Path $OutDir 'msi-work'
New-Item -ItemType Directory -Force -Path $work | Out-Null

# RTF escapes backslashes and braces; characters beyond ASCII become \uN?.
function ConvertTo-Rtf([string] $Text) {
    $builder = New-Object System.Text.StringBuilder
    foreach ($ch in $Text.Replace("`r`n", "`n").ToCharArray()) {
        $code = [int] $ch
        if ($ch -eq '\') { [void] $builder.Append('\\') }
        elseif ($ch -eq '{') { [void] $builder.Append('\{') }
        elseif ($ch -eq '}') { [void] $builder.Append('\}') }
        elseif ($ch -eq "`n") { [void] $builder.Append("\par`r`n") }
        elseif ($code -gt 127) { [void] $builder.Append('\u' + $code + '?') }
        else { [void] $builder.Append($ch) }
    }
    return $builder.ToString()
}

$mit = [System.IO.File]::ReadAllText((Join-Path $repo 'LICENSE-MIT'))
$apache = [System.IO.File]::ReadAllText((Join-Path $repo 'LICENSE-APACHE'))
$intro = "OXIM is licensed under either of the MIT license or the Apache License, Version 2.0, at your option.`n`n"
$rtf = '{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fmodern Consolas;}}\f0\fs16 ' +
    (ConvertTo-Rtf ($intro + $mit + "`n`n" + $apache)) + '}'
$license = Join-Path $work 'license.rtf'
[System.IO.File]::WriteAllText($license, $rtf, (New-Object System.Text.ASCIIEncoding))

$extensions = & wix extension list 2>$null
if (-not ($extensions -match 'WixToolset.UI.wixext')) {
    & wix extension add $UiExtension
    if ($LASTEXITCODE -ne 0) { throw "cannot add the WiX extension $UiExtension" }
}

$msi = Join-Path $OutDir "oxim-$numeric-windows-$Arch.msi"
& wix build (Join-Path $PSScriptRoot 'oxim.wxs') `
    -arch $Arch `
    -ext WixToolset.UI.wixext `
    -d "Version=$numeric" `
    -d "BinDir=$BinDir" `
    -d "RepoDir=$repo" `
    -d "LicenseRtf=$license" `
    -intermediatefolder (Join-Path $work 'obj') `
    -o $msi
if ($LASTEXITCODE -ne 0) { throw 'wix build failed' }

Write-Host "created $msi"
