[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Installer,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $ExpectedVersion
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true') {
    throw 'Release installer verification may run only on an isolated GitHub Actions Windows runner.'
}

$setup = Get-Item -LiteralPath $Installer -ErrorAction Stop
& "$PSScriptRoot/verify-windows-signatures.ps1" -Path $setup.FullName

$dataRoot = Join-Path $env:LOCALAPPDATA 'LSDJ'
if (Test-Path -LiteralPath $dataRoot) {
    throw "The release verification runner is not clean: $dataRoot already exists."
}
$app = Join-Path $dataRoot 'lsdj-app.exe'
$uninstaller = Join-Path $dataRoot 'uninstall.exe'
$sentinel = Join-Path $dataRoot 'data\release-preservation.txt'

function Invoke-ReleaseProcess {
    param([string] $FilePath, [string[]] $ArgumentList)

    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "Release lifecycle command exited $($process.ExitCode): $FilePath"
    }
}

Invoke-ReleaseProcess $setup.FullName @('/S')
if ((Get-Item -LiteralPath $app).VersionInfo.ProductVersion -ne $ExpectedVersion) {
    throw "Installed release does not report version $ExpectedVersion."
}
$payloads = @(
    Get-ChildItem -LiteralPath $dataRoot -Recurse -File |
        Where-Object { $_.Extension -in @('.exe', '.dll') } |
        ForEach-Object FullName
)
if ($payloads.Count -lt 2) {
    throw 'Expected at least the signed app and uninstaller payloads.'
}
& "$PSScriptRoot/verify-windows-signatures.ps1" -Path $payloads

New-Item -ItemType Directory -Path (Split-Path -Parent $sentinel) -Force | Out-Null
[System.IO.File]::WriteAllText($sentinel, 'preserve')
Invoke-ReleaseProcess $uninstaller @('/S')
Start-Sleep -Milliseconds 500
if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
    throw 'Default release uninstall did not preserve app-owned data.'
}
if (Test-Path -LiteralPath $app) {
    throw 'Default release uninstall left the app binary behind.'
}

Invoke-ReleaseProcess $setup.FullName @('/S')
Invoke-ReleaseProcess $uninstaller @('/S', '/PURGE-LSDJ-DATA')
Start-Sleep -Milliseconds 500
if (Test-Path -LiteralPath $dataRoot) {
    throw 'Explicit release data removal did not remove the app-owned data root.'
}
$workers = @(Get-Process -Name 'lsdj-app' -ErrorAction SilentlyContinue)
if ($workers.Count -ne 0) {
    throw "Release install/uninstall left worker processes running: $($workers.Name -join ', ')"
}
Write-Host 'Signed Windows release installer and installed payloads verified.'
