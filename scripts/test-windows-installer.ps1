[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $OlderInstaller,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $NewerInstaller,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $OlderVersion,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $NewerVersion,

    [switch] $RequireSigned
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true') {
    throw 'The destructive installer lifecycle smoke test may run only on an isolated GitHub Actions Windows runner.'
}
if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
    throw 'The Windows shipping smoke test requires an x64 OS and process.'
}

$older = Get-Item -LiteralPath $OlderInstaller -ErrorAction Stop
$newer = Get-Item -LiteralPath $NewerInstaller -ErrorAction Stop
foreach ($installer in @($older, $newer)) {
    if ($installer.PSIsContainer -or ($installer.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Installer must be a plain file: $($installer.FullName)"
    }
    if ($installer.Extension -ne '.exe' -or $installer.VersionInfo.ProductName -ne 'LSDJ') {
        throw "Installer has unexpected Windows version metadata: $($installer.FullName)"
    }
}
if ($older.VersionInfo.ProductVersion -ne $OlderVersion) {
    throw "Older installer metadata is $($older.VersionInfo.ProductVersion), expected $OlderVersion."
}
if ($newer.VersionInfo.ProductVersion -ne $NewerVersion) {
    throw "Newer installer metadata is $($newer.VersionInfo.ProductVersion), expected $NewerVersion."
}

$dataRoot = Join-Path $env:LOCALAPPDATA 'LSDJ'
$expectedRoot = [System.IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'LSDJ'))
if ([System.IO.Path]::GetFullPath($dataRoot) -cne $expectedRoot -or (Split-Path -Leaf $dataRoot) -cne 'LSDJ') {
    throw "Refusing to test an unexpected data root: $dataRoot"
}
if (Test-Path -LiteralPath $dataRoot) {
    throw "The isolated runner is not clean; refusing to overwrite existing LSDJ data at $dataRoot."
}

$app = Join-Path $dataRoot 'lsdj-app.exe'
$uninstaller = Join-Path $dataRoot 'uninstall.exe'
$startMenuShortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\LSDJ\LSDJ.lnk'
$registryKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\LSDJ'

function Invoke-CheckedProcess {
    param(
        [Parameter(Mandatory = $true)]
        [string] $FilePath,

        [string[]] $ArgumentList = @(),

        [int[]] $ExpectedExitCodes = @(0)
    )

    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -Wait -PassThru
    if ($process.ExitCode -notin $ExpectedExitCodes) {
        throw "Process exited $($process.ExitCode), expected $($ExpectedExitCodes -join ', '): $FilePath $($ArgumentList -join ' ')"
    }
    return $process.ExitCode
}

function Invoke-ExpectedFailure {
    param(
        [Parameter(Mandatory = $true)]
        [string] $FilePath,

        [string[]] $ArgumentList = @()
    )

    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -Wait -PassThru
    if ($process.ExitCode -eq 0) {
        throw "Process unexpectedly succeeded: $FilePath $($ArgumentList -join ' ')"
    }
    return $process.ExitCode
}

function Require-InstalledVersion {
    param([string] $Version)

    if (-not (Test-Path -LiteralPath $app -PathType Leaf)) {
        throw "Installed app is missing: $app"
    }
    $actual = (Get-Item -LiteralPath $app).VersionInfo.ProductVersion
    if ($actual -ne $Version) {
        throw "Installed app version is $actual, expected $Version."
    }
    $registered = (Get-ItemProperty -LiteralPath $registryKey -Name DisplayVersion).DisplayVersion
    if ($registered -ne $Version) {
        throw "Registered app version is $registered, expected $Version."
    }
}

function Require-No-Workers {
    $remaining = @(Get-Process -Name 'lsdj-app', 'lsdj_backend' -ErrorAction SilentlyContinue)
    if ($remaining.Count -ne 0) {
        throw "Installer lifecycle left LSDJ processes running: $($remaining.Name -join ', ')"
    }
}

# Initial per-user install: version metadata, Start menu integration, and the
# marker that scopes the optional destructive uninstall.
Invoke-CheckedProcess $older.FullName @('/S')
Require-InstalledVersion $OlderVersion
if (-not (Test-Path -LiteralPath $startMenuShortcut -PathType Leaf)) {
    throw "Start menu shortcut is missing: $startMenuShortcut"
}
if (-not (Test-Path -LiteralPath (Join-Path $dataRoot '.lsdj-data-root') -PathType Leaf)) {
    throw 'Installer did not create the data-root ownership marker.'
}

$settingsSentinel = Join-Path $dataRoot 'data\用户 settings\preserve.txt'
$modelSentinel = Join-Path $dataRoot 'assets\models with spaces\模型.bin'
New-Item -ItemType Directory -Path (Split-Path -Parent $settingsSentinel) -Force | Out-Null
New-Item -ItemType Directory -Path (Split-Path -Parent $modelSentinel) -Force | Out-Null
[System.IO.File]::WriteAllText($settingsSentinel, 'preserve settings')
[System.IO.File]::WriteAllText($modelSentinel, 'preserve model')

# Upgrade in place preserves app-managed data; the old signed/unsigned binary is
# replaced and registry metadata follows the newer calendar version.
Invoke-CheckedProcess $newer.FullName @('/S', '/UPDATE')
Require-InstalledVersion $NewerVersion
foreach ($sentinel in @($settingsSentinel, $modelSentinel)) {
    if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
        throw "Upgrade removed preserved data: $sentinel"
    }
}

# allowDowngrades=false must reject unattended rollback and leave the newer app.
Invoke-ExpectedFailure $older.FullName @('/S') | Out-Null
Require-InstalledVersion $NewerVersion

if ($RequireSigned) {
    $installedPayloads = @(
        Get-ChildItem -LiteralPath $dataRoot -Recurse -File |
            Where-Object { $_.Extension -in @('.exe', '.dll') } |
            ForEach-Object FullName
    )
    if ($installedPayloads.Count -eq 0) {
        throw 'The installed release contains no executable payloads to verify.'
    }
    & "$PSScriptRoot/verify-windows-signatures.ps1" -Path $installedPayloads
}

# The default uninstall removes application binaries and shortcuts but preserves
# every app-owned runtime, model, setting, and user-data file.
Invoke-CheckedProcess $uninstaller @('/S')
Start-Sleep -Milliseconds 500
Require-No-Workers
if (Test-Path -LiteralPath $app) {
    throw 'Default uninstall left the application binary behind.'
}
if (Test-Path -LiteralPath $startMenuShortcut) {
    throw 'Default uninstall left the Start menu shortcut behind.'
}
foreach ($sentinel in @($settingsSentinel, $modelSentinel)) {
    if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
        throw "Default uninstall removed user data: $sentinel"
    }
}

# An invalid marker must make explicit automation fail closed while preserving
# the exact root. Restore the installer-owned marker only after proving refusal.
Invoke-CheckedProcess $newer.FullName @('/S')
[System.IO.File]::WriteAllText((Join-Path $dataRoot '.lsdj-data-root'), 'foreign-owner')
Invoke-ExpectedFailure $uninstaller @('/S', '/PURGE-LSDJ-DATA') | Out-Null
if (-not (Test-Path -LiteralPath $dataRoot -PathType Container)) {
    throw 'Invalid ownership marker allowed explicit data removal.'
}
[System.IO.File]::WriteAllText((Join-Path $dataRoot '.lsdj-data-root'), 'works.protocol.lsdj')

# Explicit automation opt-in mirrors the GUI checkbox + path/size confirmation.
Invoke-CheckedProcess $newer.FullName @('/S')
Invoke-CheckedProcess $uninstaller @('/S', '/PURGE-LSDJ-DATA')
Start-Sleep -Milliseconds 500
if (Test-Path -LiteralPath $dataRoot) {
    throw "Explicit data removal did not remove $dataRoot."
}
Require-No-Workers

# A non-default install location with spaces, Unicode, and a long (but pre-MAX_PATH)
# directory proves package resources do not depend on Windows long-path support.
$longLeaf = ('path segment ' * 10).Trim()
$unicodeInstall = Join-Path $env:RUNNER_TEMP "LSDJ installer 路径 $longLeaf"
if ($unicodeInstall.Length -ge 240) {
    throw "The long-path-disabled smoke target exceeded its conservative budget: $($unicodeInstall.Length)"
}
Invoke-CheckedProcess $newer.FullName @('/S', "/D=$unicodeInstall")
$unicodeApp = Join-Path $unicodeInstall 'lsdj-app.exe'
$unicodeUninstaller = Join-Path $unicodeInstall 'uninstall.exe'
if (-not (Test-Path -LiteralPath $unicodeApp -PathType Leaf)) {
    throw "Unicode/space install did not produce the app at $unicodeApp."
}
Invoke-CheckedProcess $unicodeUninstaller @('/S')
Start-Sleep -Milliseconds 500
if (Test-Path -LiteralPath $unicodeApp) {
    throw 'Unicode/space uninstall left the app binary behind.'
}

# The custom-location uninstall intentionally retains its remembered location.
# Override it explicitly so the final purge cleans the isolated runner's normal
# application/data root as well as the remembered-location registry state.
Invoke-CheckedProcess $newer.FullName @('/S', "/D=$dataRoot")
Invoke-CheckedProcess $uninstaller @('/S', '/PURGE-LSDJ-DATA')
Start-Sleep -Milliseconds 500
if (Test-Path -LiteralPath $dataRoot) {
    throw 'Final explicit cleanup did not remove the LSDJ data root.'
}
Require-No-Workers
Write-Host 'Windows NSIS install/upgrade/downgrade/uninstall lifecycle passed.'
