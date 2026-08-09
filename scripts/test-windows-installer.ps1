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
$marker = Join-Path $dataRoot '.lsdj-data-root'
$startMenuShortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\LSDJ\LSDJ.lnk'
$registryKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\LSDJ'
$ciPurgeReady = Join-Path $env:TEMP 'lsdj-ci-before-purge.ready'
$ciInstallerTrace = Join-Path $env:TEMP 'lsdj-ci-installer.trace'
$ownerMarkerWithNul = [byte[]]::new(20)
[Text.Encoding]::ASCII.GetBytes('works.protocol.lsdj').CopyTo($ownerMarkerWithNul, 0)
$ownerMarkerWithNulHex = [Convert]::ToHexString($ownerMarkerWithNul)

function Get-CiInstallerTrace {
    if (Test-Path -LiteralPath $ciInstallerTrace -PathType Leaf) {
        return [System.IO.File]::ReadAllText($ciInstallerTrace)
    }
    return '<no CI installer trace was written>'
}

function Write-CiInstallerTrace {
    Write-Host 'CI installer trace:'
    if (Test-Path -LiteralPath $ciInstallerTrace -PathType Leaf) {
        Get-Content -LiteralPath $ciInstallerTrace | ForEach-Object {
            Write-Host "  $_"
        }
    } else {
        Write-Host '  <no CI installer trace was written>'
    }
}

function Assert-CiInstallerTraceContract {
    param(
        [string[]] $Required = @(),

        [string[]] $Forbidden = @()
    )

    # Signed release installers compile every trace macro to no instructions.
    if ($RequireSigned) {
        return
    }

    $trace = Get-CiInstallerTrace
    $lastRequiredIndex = -1
    foreach ($needle in $Required) {
        $requiredIndex = $trace.IndexOf(
            $needle,
            $lastRequiredIndex + 1,
            [StringComparison]::Ordinal
        )
        if ($requiredIndex -lt 0) {
            Write-CiInstallerTrace
            throw "CI installer trace is missing or misorders required checkpoint: $needle"
        }
        $lastRequiredIndex = $requiredIndex
    }
    foreach ($needle in $Forbidden) {
        if ($trace.IndexOf($needle, [StringComparison]::Ordinal) -ge 0) {
            Write-CiInstallerTrace
            throw "CI installer trace reached forbidden checkpoint: $needle"
        }
    }
}

function Invoke-CheckedProcess {
    param(
        [Parameter(Mandatory = $true)]
        [string] $FilePath,

        [string[]] $ArgumentList = @(),

        [int[]] $ExpectedExitCodes = @(0)
    )

    Remove-Item -LiteralPath $ciInstallerTrace -Force -ErrorAction SilentlyContinue
    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -Wait -PassThru
    if ($process.ExitCode -notin $ExpectedExitCodes) {
        $trace = Get-CiInstallerTrace
        Write-CiInstallerTrace
        throw "Process exited $($process.ExitCode), expected $($ExpectedExitCodes -join ', '): $FilePath $($ArgumentList -join ' ')`nCI installer trace:`n$trace"
    }
    return $process.ExitCode
}

function Invoke-ExpectedFailure {
    param(
        [Parameter(Mandatory = $true)]
        [string] $FilePath,

        [string[]] $ArgumentList = @()
    )

    Remove-Item -LiteralPath $ciInstallerTrace -Force -ErrorAction SilentlyContinue
    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -Wait -PassThru
    if ($process.ExitCode -eq 0) {
        $trace = Get-CiInstallerTrace
        Write-CiInstallerTrace
        throw "Process unexpectedly succeeded: $FilePath $($ArgumentList -join ' ')`nCI installer trace:`n$trace"
    }
    return $process.ExitCode
}

function New-UninstallerWorkerCopy {
    param(
        [Parameter(Mandatory = $true)]
        [string] $FilePath
    )

    $workerName = "lsdj-uninstall-worker-$([Guid]::NewGuid().ToString('N')).exe"
    $workerPath = Join-Path $env:RUNNER_TEMP $workerName
    try {
        Copy-Item -LiteralPath $FilePath -Destination $workerPath
        return $workerPath
    } catch {
        Remove-Item -LiteralPath $workerPath -Force -ErrorAction SilentlyContinue
        throw
    }
}

function Stop-UninstallerWorker {
    param(
        [System.Diagnostics.Process] $Process,

        [int] $TimeoutMilliseconds = 10000
    )

    if ($null -eq $Process -or $Process.HasExited) {
        return
    }

    $Process.Kill($true)
    if (-not $Process.WaitForExit($TimeoutMilliseconds)) {
        throw "Timed out terminating uninstaller worker process $($Process.Id)."
    }
}

function Invoke-ExpectedUninstallFailure {
    param(
        [Parameter(Mandatory = $true)]
        [string] $FilePath,

        [Parameter(Mandatory = $true)]
        [string] $InstallDirectory,

        [string[]] $ArgumentList = @()
    )

    # NSIS's installed uninstaller is only a self-copy launcher: its exit code
    # reports whether the temporary worker started, not the worker's result.
    # Exercise the documented worker form so CI observes the script exit code.
    # https://nsis.sourceforge.io/Docs/AppendixD.html
    # `_?=` must remain the final, unquoted command-line argument.
    $workerPath = $null
    $worker = $null
    try {
        $workerPath = New-UninstallerWorkerCopy -FilePath $FilePath
        Remove-Item -LiteralPath $ciInstallerTrace -Force -ErrorAction SilentlyContinue
        $worker = Start-Process `
            -FilePath $workerPath `
            -ArgumentList (@($ArgumentList) + "_?=$InstallDirectory") `
            -PassThru
        if (-not $worker.WaitForExit(30000)) {
            throw "Timed out waiting for uninstaller worker: $workerPath"
        }
        if ($worker.ExitCode -ne 2) {
            $trace = Get-CiInstallerTrace
            Write-CiInstallerTrace
            throw "Uninstaller worker exited $($worker.ExitCode), expected 2: $workerPath$([Environment]::NewLine)CI installer trace:$([Environment]::NewLine)$trace"
        }
        return $worker.ExitCode
    } finally {
        try {
            Stop-UninstallerWorker -Process $worker
        } finally {
            if ($null -ne $workerPath) {
                Remove-Item -LiteralPath $workerPath -Force -ErrorAction SilentlyContinue
            }
        }
    }
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

function Set-DisplayVersionEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $Value
    )

    New-ItemProperty -LiteralPath $registryKey -Name DisplayVersion -Value $Value `
        -PropertyType String -Force | Out-Null
    $actual = (Get-ItemProperty -LiteralPath $registryKey -Name DisplayVersion).DisplayVersion
    $rawKey = Get-Item -LiteralPath $registryKey -ErrorAction Stop
    $rawKind = $rawKey.GetValueKind('DisplayVersion')
    $rawValue = $rawKey.GetValue(
        'DisplayVersion',
        $null,
        [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    )
    if ($actual -cne $Value -or
        $rawKind -ne [Microsoft.Win32.RegistryValueKind]::String -or
        $rawValue -cne $Value) {
        throw "Could not establish exact DisplayVersion test evidence: $Value"
    }
}

function Require-No-Workers {
    # Runtime programs are independently declared by the MRT2 and SA3 service
    # manifests and do not have one stable process name. The installer lifecycle
    # never launches the app, so observing the shell is the authoritative leak.
    $remaining = @(Get-Process -Name 'lsdj-app' -ErrorAction SilentlyContinue)
    if ($remaining.Count -ne 0) {
        throw "Installer lifecycle left LSDJ processes running: $($remaining.Name -join ', ')"
    }
}

function Get-UninstallRegistrySnapshot {
    $key = Get-Item -LiteralPath $registryKey -ErrorAction Stop
    $values = foreach ($name in @($key.GetValueNames() | Sort-Object)) {
        $value = $key.GetValue(
            $name,
            $null,
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        if ($value -is [byte[]]) {
            $value = [Convert]::ToBase64String($value)
        } elseif ($value -is [string[]]) {
            $value = $value -join "`0"
        } else {
            $value = [string] $value
        }
        [ordered]@{
            Name = $name
            Kind = [string] ($key.GetValueKind($name))
            Value = $value
        }
    }
    return (ConvertTo-Json -InputObject @($values) -Compress)
}

function Get-InstalledStateSnapshot {
    foreach ($path in @($app, $uninstaller, $marker, $settingsSentinel, $modelSentinel)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Cannot snapshot missing installed state: $path"
        }
    }

    $markerEntry = Get-Item -LiteralPath $marker -Force -ErrorAction Stop
    if (($markerEntry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Cannot snapshot a reparse-point ownership marker: $marker"
    }
    $markerLinkTypeProperty = $markerEntry.PSObject.Properties['LinkType']
    $markerLinkType = if ($null -eq $markerLinkTypeProperty) {
        ''
    } else {
        [string] $markerLinkTypeProperty.Value
    }
    $markerTargetProperty = $markerEntry.PSObject.Properties['Target']
    $markerTarget = if ($null -eq $markerTargetProperty -or $null -eq $markerTargetProperty.Value) {
        ''
    } elseif ($markerTargetProperty.Value -is [string[]]) {
        $markerTargetProperty.Value -join "`0"
    } else {
        [string] $markerTargetProperty.Value
    }

    return [ordered]@{
        App = (Get-FileHash -LiteralPath $app -Algorithm SHA256).Hash
        Uninstaller = (Get-FileHash -LiteralPath $uninstaller -Algorithm SHA256).Hash
        Marker = (Get-FileHash -LiteralPath $marker -Algorithm SHA256).Hash
        MarkerAttributes = [int64] $markerEntry.Attributes
        MarkerLinkType = $markerLinkType
        MarkerTarget = $markerTarget
        Settings = (Get-FileHash -LiteralPath $settingsSentinel -Algorithm SHA256).Hash
        Model = (Get-FileHash -LiteralPath $modelSentinel -Algorithm SHA256).Hash
        Registry = (Get-UninstallRegistrySnapshot)
    }
}

function Assert-InstalledStateSnapshotUnchanged {
    param(
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary] $Before
    )

    $after = Get-InstalledStateSnapshot
    foreach ($name in $Before.Keys) {
        if ($after[$name] -cne $Before[$name]) {
            throw "Rejected installer changed installed state: $name"
        }
    }
}

function Remove-ReparseDirectoryEntry {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    $entry = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or -not $entry.PSIsContainer) {
        throw "Refusing to unlink an entry that is not a directory reparse point: $Path"
    }
    [System.IO.Directory]::Delete($entry.FullName)
}

function New-RecognizedLsdjLayout {
    New-Item -ItemType Directory -Path $dataRoot -Force | Out-Null
    foreach ($name in @('config', 'data', 'cache', 'assets', 'staging')) {
        New-Item -ItemType Directory -Path (Join-Path $dataRoot $name) -Force | Out-Null
    }
}

function Start-LifecycleScenario {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Name
    )

    Write-Host "[Windows installer lifecycle] $Name"
}

# An empty foreign root is still not evidence of ownership. This also proves
# the hook's hidden root probe runs before Tauri's path-creating SetOutPath:
# otherwise fresh and pre-existing empty roots would be indistinguishable.
Start-LifecycleScenario 'reject pre-existing empty data root'
New-Item -ItemType Directory -Path $dataRoot -Force | Out-Null
Invoke-ExpectedFailure $older.FullName @('/S') | Out-Null
if ((Test-Path -LiteralPath $marker) -or (Test-Path -LiteralPath $app)) {
    throw 'Installer claimed a pre-existing empty LocalAppData root.'
}
Remove-Item -LiteralPath $dataRoot -Recurse -Force

# A foreign pre-existing directory must never be claimed just because it has the
# expected basename. The failed installer must not add a marker or payload.
Start-LifecycleScenario 'reject foreign pre-existing data root'
New-Item -ItemType Directory -Path $dataRoot -Force | Out-Null
$foreignSentinel = Join-Path $dataRoot 'foreign-owner.txt'
[System.IO.File]::WriteAllText($foreignSentinel, 'not LSDJ')
Invoke-ExpectedFailure $older.FullName @('/S') | Out-Null
if (-not (Test-Path -LiteralPath $foreignSentinel -PathType Leaf) -or
    (Test-Path -LiteralPath $marker) -or
    (Test-Path -LiteralPath $app)) {
    throw 'Installer claimed or modified a pre-existing foreign LocalAppData root.'
}
Remove-Item -LiteralPath $dataRoot -Recurse -Force

# A root junction must be rejected before either its target or an ownership
# marker is touched.
Start-LifecycleScenario 'reject install-time data-root junction'
$rootJunctionTarget = Join-Path $env:RUNNER_TEMP 'lsdj-root-junction-target'
New-Item -ItemType Directory -Path $rootJunctionTarget -Force | Out-Null
$rootJunctionSentinel = Join-Path $rootJunctionTarget 'outside.txt'
[System.IO.File]::WriteAllText($rootJunctionSentinel, 'outside root')
New-Item -ItemType Junction -Path $dataRoot -Target $rootJunctionTarget | Out-Null
Invoke-ExpectedFailure $older.FullName @('/S') | Out-Null
if (-not (Test-Path -LiteralPath $rootJunctionSentinel -PathType Leaf) -or
    (Test-Path -LiteralPath (Join-Path $rootJunctionTarget '.lsdj-data-root'))) {
    throw 'Installer followed or marked a LocalAppData root junction.'
}
Remove-ReparseDirectoryEntry $dataRoot
Remove-Item -LiteralPath $rootJunctionTarget -Recurse -Force

# Even an otherwise recognizable legacy layout is unsafe when its marker entry
# is a junction/reparse point.
Start-LifecycleScenario 'reject install-time ownership-marker junction'
New-RecognizedLsdjLayout
$installMarkerTarget = Join-Path $env:RUNNER_TEMP 'lsdj-install-marker-target'
New-Item -ItemType Directory -Path $installMarkerTarget -Force | Out-Null
$installMarkerSentinel = Join-Path $installMarkerTarget 'outside.txt'
[System.IO.File]::WriteAllText($installMarkerSentinel, 'outside marker')
New-Item -ItemType Junction -Path $marker -Target $installMarkerTarget | Out-Null
Invoke-ExpectedFailure $older.FullName @('/S') | Out-Null
if (-not (Test-Path -LiteralPath $installMarkerSentinel -PathType Leaf) -or
    (Test-Path -LiteralPath $app)) {
    throw 'Installer followed or accepted a marker reparse point.'
}
Remove-ReparseDirectoryEntry $marker
Remove-Item -LiteralPath $dataRoot -Recurse -Force
Remove-Item -LiteralPath $installMarkerTarget -Recurse -Force

# A recognizable five-root shell is not safe to adopt if anything nested below
# it is a junction. Installation must not mark or write through the link.
Start-LifecycleScenario 'reject nested junction during legacy adoption'
New-RecognizedLsdjLayout
$installNestedTarget = Join-Path $env:RUNNER_TEMP 'lsdj-install-nested-target'
New-Item -ItemType Directory -Path $installNestedTarget -Force | Out-Null
$installNestedSentinel = Join-Path $installNestedTarget 'outside.txt'
[System.IO.File]::WriteAllText($installNestedSentinel, 'outside nested install')
$installNestedJunction = Join-Path $dataRoot 'assets\linked-outside'
New-Item -ItemType Junction -Path $installNestedJunction -Target $installNestedTarget | Out-Null
Invoke-ExpectedFailure $older.FullName @('/S') | Out-Null
if (-not (Test-Path -LiteralPath $installNestedSentinel -PathType Leaf) -or
    (Test-Path -LiteralPath $marker) -or
    (Test-Path -LiteralPath $app)) {
    throw 'Installer adopted or followed a nested directory reparse point.'
}
Remove-ReparseDirectoryEntry $installNestedJunction
Remove-Item -LiteralPath $dataRoot -Recurse -Force
Remove-Item -LiteralPath $installNestedTarget -Recurse -Force

# The native validator reads one byte beyond the exact owner identifier. This
# rejects an embedded trailing NUL even though string conversion alone could
# make that 20-byte file compare equal to the expected 19-character text.
Start-LifecycleScenario 'reject install with NUL-extended ownership marker'
New-RecognizedLsdjLayout
$nulInstallSentinel = Join-Path $dataRoot 'data\nul-marker-install.txt'
[System.IO.File]::WriteAllText($nulInstallSentinel, 'preserve NUL marker root')
[System.IO.File]::WriteAllBytes($marker, $ownerMarkerWithNul)
Invoke-ExpectedFailure $older.FullName @('/S') | Out-Null
$actualNulMarkerHex = [Convert]::ToHexString([System.IO.File]::ReadAllBytes($marker))
if ($actualNulMarkerHex -cne $ownerMarkerWithNulHex -or
    -not (Test-Path -LiteralPath $nulInstallSentinel -PathType Leaf) -or
    (Test-Path -LiteralPath $app)) {
    throw 'Installer accepted or modified a NUL-extended ownership marker root.'
}
Remove-Item -LiteralPath $dataRoot -Recurse -Force

# The one markerless migration case is the complete five-root layout created by
# platform_paths.rs. It may be adopted, upgraded, and preserved normally.
Start-LifecycleScenario 'adopt recognized markerless legacy layout'
New-RecognizedLsdjLayout
$legacySentinel = Join-Path $dataRoot 'data\recognized-layout.txt'
[System.IO.File]::WriteAllText($legacySentinel, 'recognized LSDJ layout')
Invoke-CheckedProcess $older.FullName @('/S')
if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
    throw 'Installer did not establish ownership for a recognized LSDJ layout.'
}
Invoke-CheckedProcess $uninstaller @('/S')
if (-not (Test-Path -LiteralPath $legacySentinel -PathType Leaf)) {
    throw 'Default uninstall did not preserve an adopted LSDJ layout.'
}
Remove-Item -LiteralPath $dataRoot -Recurse -Force

# Initial per-user install: version metadata, Start menu integration, and the
# marker that scopes the optional destructive uninstall.
Start-LifecycleScenario 'fresh per-user install'
Invoke-CheckedProcess $older.FullName @('/S')
Require-InstalledVersion $OlderVersion
if (-not (Test-Path -LiteralPath $startMenuShortcut -PathType Leaf)) {
    throw "Start menu shortcut is missing: $startMenuShortcut"
}
if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
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
Start-LifecycleScenario 'upgrade in place and preserve app-managed data'
Invoke-CheckedProcess $newer.FullName @('/S', '/UPDATE')
Assert-CiInstallerTraceContract -Required @(
    "preinstall: installed version=$OlderVersion validity=1",
    'preinstall: version compare=1',
    'preinstall: ready',
    'postinstall: begin'
) -Forbidden @('abort:')
Require-InstalledVersion $NewerVersion
foreach ($sentinel in @($settingsSentinel, $modelSentinel)) {
    if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
        throw "Upgrade removed preserved data: $sentinel"
    }
}
Require-No-Workers

$registrySentinelName = 'LsdjCiPreserve'
$registrySentinelValue = 'preserve registry metadata'
New-ItemProperty -LiteralPath $registryKey -Name $registrySentinelName `
    -Value $registrySentinelValue -PropertyType String -Force | Out-Null
$registryBaseline = Get-ItemProperty -LiteralPath $registryKey

function Require-RejectedInstallPreservedState {
    param(
        [AllowEmptyString()]
        [string] $ExpectedDisplayVersion = $NewerVersion,

        [switch] $DisplayVersionMissing
    )

    if (-not (Test-Path -LiteralPath $app -PathType Leaf)) {
        throw "Rejected installer removed the installed app: $app"
    }
    $actualAppVersion = (Get-Item -LiteralPath $app).VersionInfo.ProductVersion
    if ($actualAppVersion -ne $NewerVersion) {
        throw "Rejected installer changed app version to $actualAppVersion."
    }

    $registered = Get-ItemProperty -LiteralPath $registryKey -ErrorAction Stop
    $displayVersionProperty = $registered.PSObject.Properties['DisplayVersion']
    if ($DisplayVersionMissing) {
        if ($null -ne $displayVersionProperty) {
            throw 'Rejected installer recreated missing DisplayVersion metadata.'
        }
    } elseif ($null -eq $displayVersionProperty -or
        $displayVersionProperty.Value -cne $ExpectedDisplayVersion) {
        throw "Rejected installer changed DisplayVersion metadata."
    }

    foreach ($name in @('DisplayName', 'InstallLocation', 'UninstallString', $registrySentinelName)) {
        if ($registered.$name -cne $registryBaseline.$name) {
            throw "Rejected installer changed registry metadata: $name"
        }
    }
    foreach ($sentinel in @($settingsSentinel, $modelSentinel)) {
        if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
            throw "Rejected installer modified app-managed data: $sentinel"
        }
    }
    Require-No-Workers
}

$forbiddenPostVersionGuardTrace = @(
    'preinstall: begin',
    'preinstall: canonical',
    'preinstall: ready',
    'postinstall:'
)

# A same-version unattended reinstall remains supported. Besides exercising the
# success path, its trace directly proves the pinned plugin reports valid=>1.
Start-LifecycleScenario 'same-version silent reinstall'
Invoke-CheckedProcess $newer.FullName @('/S')
Assert-CiInstallerTraceContract -Required @(
    "preinstall: installed version=$NewerVersion validity=1",
    'preinstall: version compare=0',
    'preinstall: ready',
    'postinstall: begin'
) -Forbidden @('abort:')
Require-InstalledVersion $NewerVersion
foreach ($sentinel in @($settingsSentinel, $modelSentinel)) {
    if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
        throw "Same-version reinstall modified app-managed data: $sentinel"
    }
}
Require-No-Workers

# allowDowngrades=false must reject unattended rollback and leave the newer app.
Start-LifecycleScenario 'reject unattended downgrade'
$rejectedStateBefore = Get-InstalledStateSnapshot
Invoke-CheckedProcess -FilePath $older.FullName -ArgumentList @('/S') -ExpectedExitCodes @(2) | Out-Null
Assert-CiInstallerTraceContract -Required @(
    "preinstall: installed version=$NewerVersion validity=1",
    'preinstall: version compare=-1',
    'abort: unattended downgrade'
) -Forbidden $forbiddenPostVersionGuardTrace
Require-RejectedInstallPreservedState
Assert-InstalledStateSnapshotUnchanged -Before $rejectedStateBefore

# Explicit update mode must not bypass the unattended downgrade guard.
Start-LifecycleScenario 'reject unattended downgrade in update mode'
$rejectedStateBefore = Get-InstalledStateSnapshot
Invoke-CheckedProcess -FilePath $older.FullName -ArgumentList @('/S', '/UPDATE') `
    -ExpectedExitCodes @(2) | Out-Null
Assert-CiInstallerTraceContract -Required @(
    "preinstall: installed version=$NewerVersion validity=1",
    'preinstall: version compare=-1',
    'abort: unattended downgrade'
) -Forbidden $forbiddenPostVersionGuardTrace
Require-RejectedInstallPreservedState
Assert-InstalledStateSnapshotUnchanged -Before $rejectedStateBefore

# Passive mode is unattended too, despite not setting NSIS's silent flag.
Start-LifecycleScenario 'reject passive downgrade'
$passiveStateBefore = Get-InstalledStateSnapshot
Invoke-CheckedProcess -FilePath $older.FullName -ArgumentList @('/P') -ExpectedExitCodes @(2) | Out-Null
Assert-CiInstallerTraceContract -Required @(
    'abort: passive install unsupported'
) -Forbidden @('probe:', 'preinstall:', 'postinstall:')
Require-RejectedInstallPreservedState
Assert-InstalledStateSnapshotUnchanged -Before $passiveStateBefore

# NSIS silent mode suppresses GUI initialization, so a combined /S /P reaches
# the repeated PREINSTALL policy check only after the read-only ownership probe.
Start-LifecycleScenario 'reject combined silent and passive mode'
$combinedPassiveStateBefore = Get-InstalledStateSnapshot
Invoke-CheckedProcess -FilePath $older.FullName -ArgumentList @('/S', '/P') `
    -ExpectedExitCodes @(2) | Out-Null
Assert-CiInstallerTraceContract -Required @(
    'abort: passive install unsupported'
) -Forbidden @('preinstall: begin', 'preinstall: ready', 'postinstall:')
Require-RejectedInstallPreservedState
Assert-InstalledStateSnapshotUnchanged -Before $combinedPassiveStateBefore

# Existing install evidence with empty, malformed, or missing version metadata
# is unsafe. Each refusal must preserve the exact damaged evidence for repair.
Start-LifecycleScenario 'reject existing install with empty version metadata'
Set-DisplayVersionEvidence -Value ''
$rejectedStateBefore = Get-InstalledStateSnapshot
Invoke-CheckedProcess -FilePath $older.FullName -ArgumentList @('/S') -ExpectedExitCodes @(2) | Out-Null
Assert-CiInstallerTraceContract -Required @(
    'abort: installed version missing'
) -Forbidden $forbiddenPostVersionGuardTrace
Require-RejectedInstallPreservedState -ExpectedDisplayVersion ''
Assert-InstalledStateSnapshotUnchanged -Before $rejectedStateBefore

Start-LifecycleScenario 'reject existing install with corrupt version metadata'
$corruptDisplayVersion = 'not-a-semver'
Set-DisplayVersionEvidence -Value $corruptDisplayVersion
$rejectedStateBefore = Get-InstalledStateSnapshot
Invoke-CheckedProcess -FilePath $older.FullName -ArgumentList @('/S') -ExpectedExitCodes @(2) | Out-Null
Assert-CiInstallerTraceContract -Required @(
    "preinstall: version evidence installed=$corruptDisplayVersion present=1 existing=1",
    "preinstall: installed version=$corruptDisplayVersion validity=0",
    'abort: installed version invalid'
) -Forbidden @(
    'preinstall: version compare=',
    'preinstall: begin',
    'preinstall: canonical',
    'preinstall: ready',
    'postinstall:'
)
Require-RejectedInstallPreservedState -ExpectedDisplayVersion $corruptDisplayVersion
Assert-InstalledStateSnapshotUnchanged -Before $rejectedStateBefore

Start-LifecycleScenario 'reject existing install with missing version metadata'
Remove-ItemProperty -LiteralPath $registryKey -Name DisplayVersion
$rejectedStateBefore = Get-InstalledStateSnapshot
Invoke-CheckedProcess -FilePath $older.FullName -ArgumentList @('/S') -ExpectedExitCodes @(2) | Out-Null
Assert-CiInstallerTraceContract -Required @(
    'abort: installed version missing'
) -Forbidden $forbiddenPostVersionGuardTrace
Require-RejectedInstallPreservedState -DisplayVersionMissing
Assert-InstalledStateSnapshotUnchanged -Before $rejectedStateBefore
Set-DisplayVersionEvidence -Value $NewerVersion
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
Start-LifecycleScenario 'default uninstall preserves app-managed data'
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
Start-LifecycleScenario 'reject purge with invalid ownership marker'
Invoke-CheckedProcess $newer.FullName @('/S')
[System.IO.File]::WriteAllText($marker, 'foreign-owner')
Invoke-ExpectedUninstallFailure `
    -FilePath $uninstaller `
    -InstallDirectory $dataRoot `
    -ArgumentList @('/S', '/PURGE-LSDJ-DATA') | Out-Null
if (-not (Test-Path -LiteralPath $dataRoot -PathType Container)) {
    throw 'Invalid ownership marker allowed explicit data removal.'
}
[System.IO.File]::WriteAllText($marker, 'works.protocol.lsdj')

# A trailing raw NUL must also invalidate destructive ownership validation.
# The failed purge must stop before ordinary app removal and preserve the exact
# marker bytes and data root.
Start-LifecycleScenario 'reject purge with NUL-extended ownership marker'
Invoke-CheckedProcess $newer.FullName @('/S')
[System.IO.File]::WriteAllBytes($marker, $ownerMarkerWithNul)
Invoke-ExpectedUninstallFailure `
    -FilePath $uninstaller `
    -InstallDirectory $dataRoot `
    -ArgumentList @('/S', '/PURGE-LSDJ-DATA') | Out-Null
$actualNulMarkerHex = [Convert]::ToHexString([System.IO.File]::ReadAllBytes($marker))
if ($actualNulMarkerHex -cne $ownerMarkerWithNulHex -or
    -not (Test-Path -LiteralPath $dataRoot -PathType Container) -or
    -not (Test-Path -LiteralPath $app -PathType Leaf)) {
    throw 'NUL-extended ownership marker allowed uninstall or data removal.'
}
[System.IO.File]::WriteAllText($marker, 'works.protocol.lsdj')

# Replace the entire owned root with a junction immediately before explicit
# purge. The purge-time root junction check must stop before even Tauri's narrow
# payload deletion, and
# the outside target must remain byte-for-byte untouched.
Start-LifecycleScenario 'reject purge after data-root junction replacement'
Invoke-CheckedProcess $newer.FullName @('/S')
$parkedRoot = Join-Path $env:RUNNER_TEMP 'lsdj-owned-root-parked'
Move-Item -LiteralPath $dataRoot -Destination $parkedRoot
$purgeRootTarget = Join-Path $env:RUNNER_TEMP 'lsdj-purge-root-target'
New-Item -ItemType Directory -Path $purgeRootTarget -Force | Out-Null
$purgeRootMarker = Join-Path $purgeRootTarget '.lsdj-data-root'
$purgeRootSentinel = Join-Path $purgeRootTarget 'lsdj-app.exe'
[System.IO.File]::WriteAllText($purgeRootMarker, 'works.protocol.lsdj')
[System.IO.File]::WriteAllText($purgeRootSentinel, 'outside root payload')
New-Item -ItemType Junction -Path $dataRoot -Target $purgeRootTarget | Out-Null
$parkedUninstaller = Join-Path $parkedRoot 'uninstall.exe'
Invoke-ExpectedUninstallFailure `
    -FilePath $parkedUninstaller `
    -InstallDirectory $parkedRoot `
    -ArgumentList @('/S', '/PURGE-LSDJ-DATA') | Out-Null
if (([System.IO.File]::ReadAllText($purgeRootSentinel)) -ne 'outside root payload' -or
    -not (Test-Path -LiteralPath $parkedUninstaller -PathType Leaf)) {
    throw 'Purge-time root junction was followed or ordinary uninstall continued after refusal.'
}
Remove-ReparseDirectoryEntry $dataRoot
Remove-Item -LiteralPath $purgeRootTarget -Recurse -Force
Move-Item -LiteralPath $parkedRoot -Destination $dataRoot

# A marker junction is rejected both as ownership evidence and as a tree entry;
# its outside target must remain untouched.
Start-LifecycleScenario 'reject purge with ownership-marker junction'
Invoke-CheckedProcess $newer.FullName @('/S')
[System.IO.File]::Delete($marker)
$purgeMarkerTarget = Join-Path $env:RUNNER_TEMP 'lsdj-purge-marker-target'
New-Item -ItemType Directory -Path $purgeMarkerTarget -Force | Out-Null
$purgeMarkerSentinel = Join-Path $purgeMarkerTarget 'outside.txt'
[System.IO.File]::WriteAllText($purgeMarkerSentinel, 'outside purge marker')
New-Item -ItemType Junction -Path $marker -Target $purgeMarkerTarget | Out-Null
Invoke-ExpectedUninstallFailure `
    -FilePath $uninstaller `
    -InstallDirectory $dataRoot `
    -ArgumentList @('/S', '/PURGE-LSDJ-DATA') | Out-Null
if (-not (Test-Path -LiteralPath $purgeMarkerSentinel -PathType Leaf) -or
    -not (Test-Path -LiteralPath $dataRoot -PathType Container)) {
    throw 'Explicit purge followed or removed a marker reparse point.'
}
Remove-ReparseDirectoryEntry $marker
[System.IO.File]::WriteAllText($marker, 'works.protocol.lsdj')
Remove-Item -LiteralPath $purgeMarkerTarget -Recurse -Force

# Unsigned CI installers pause after the initial ownership/size decision and
# core binary removal. Replace the marker during that window; the immediate
# destructive revalidation must detect the change and preserve the root.
Start-LifecycleScenario 'reject ownership-marker replacement after purge confirmation'
Invoke-CheckedProcess $newer.FullName @('/S')
Remove-Item -LiteralPath $ciPurgeReady -Force -ErrorAction SilentlyContinue
$racedPurgeWorker = $null
$racedPurge = $null
try {
    $racedPurgeWorker = New-UninstallerWorkerCopy -FilePath $uninstaller
    $racedPurge = Start-Process -FilePath $racedPurgeWorker `
        -ArgumentList @(
            '/S',
            '/PURGE-LSDJ-DATA',
            '/LSDJ-CI-PAUSE-BEFORE-PURGE',
            "_?=$dataRoot"
        ) `
        -PassThru
    $raceDeadline = [DateTime]::UtcNow.AddSeconds(20)
    while (-not (Test-Path -LiteralPath $ciPurgeReady -PathType Leaf) -and
        -not $racedPurge.HasExited) {
        if ([DateTime]::UtcNow -ge $raceDeadline) {
            throw 'Timed out waiting for the CI purge synchronization point.'
        }
        Start-Sleep -Milliseconds 100
    }
    if ($racedPurge.HasExited) {
        throw "Purge exited before the marker-replacement test (exit $($racedPurge.ExitCode))."
    }
    [System.IO.File]::WriteAllText($marker, 'replaced-after-confirmation')
    if (-not $racedPurge.WaitForExit(30000)) {
        throw 'Timed out waiting for the refused marker-replacement purge to exit.'
    }
    if ($racedPurge.ExitCode -ne 2 -or
        -not (Test-Path -LiteralPath $dataRoot -PathType Container)) {
        throw 'Marker replacement after confirmation did not fail closed.'
    }
} finally {
    try {
        Stop-UninstallerWorker -Process $racedPurge
    } finally {
        try {
            if ($null -ne $racedPurgeWorker) {
                Remove-Item -LiteralPath $racedPurgeWorker -Force -ErrorAction SilentlyContinue
            }
        } finally {
            Remove-Item -LiteralPath $ciPurgeReady -Force -ErrorAction SilentlyContinue
        }
    }
}
[System.IO.File]::WriteAllText($marker, 'works.protocol.lsdj')

# Nested junctions are never traversed for size or removal. Purge refuses the
# tree and leaves both the root and outside target intact.
Start-LifecycleScenario 'reject purge with nested junction'
Invoke-CheckedProcess $newer.FullName @('/S')
$nestedTarget = Join-Path $env:RUNNER_TEMP 'lsdj-nested-junction-target'
New-Item -ItemType Directory -Path $nestedTarget -Force | Out-Null
$nestedSentinel = Join-Path $nestedTarget 'outside.txt'
[System.IO.File]::WriteAllText($nestedSentinel, 'outside nested junction')
$nestedJunction = Join-Path $dataRoot 'data\linked-outside'
New-Item -ItemType Junction -Path $nestedJunction -Target $nestedTarget | Out-Null
Invoke-ExpectedUninstallFailure `
    -FilePath $uninstaller `
    -InstallDirectory $dataRoot `
    -ArgumentList @('/S', '/PURGE-LSDJ-DATA') | Out-Null
if (-not (Test-Path -LiteralPath $nestedSentinel -PathType Leaf) -or
    -not (Test-Path -LiteralPath $dataRoot -PathType Container)) {
    throw 'Explicit purge traversed a nested directory reparse point.'
}
Remove-ReparseDirectoryEntry $nestedJunction
Remove-Item -LiteralPath $nestedTarget -Recurse -Force

# Explicit automation opt-in mirrors the GUI checkbox + path/size confirmation.
Start-LifecycleScenario 'explicit purge removes owned data root'
Invoke-CheckedProcess $newer.FullName @('/S')
Invoke-CheckedProcess $uninstaller @('/S', '/PURGE-LSDJ-DATA')
Start-Sleep -Milliseconds 500
if (Test-Path -LiteralPath $dataRoot) {
    throw "Explicit data removal did not remove $dataRoot."
}
Require-No-Workers

# A non-default install location with spaces, Unicode, and a long (but pre-MAX_PATH)
# directory proves package resources do not depend on Windows long-path support.
Start-LifecycleScenario 'custom install path with spaces Unicode and long leaf'
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

# Exercise NSIS's documented worker form with a final, unquoted `_?=` value
# whose remainder contains spaces and Unicode. The unsafe purge must expose
# exact worker exit 2 and preserve both the custom app and owned data root.
Start-LifecycleScenario 'reject purge worker with spaces and Unicode install path'
[System.IO.File]::WriteAllText($marker, 'foreign-owner')
Invoke-ExpectedUninstallFailure `
    -FilePath $unicodeUninstaller `
    -InstallDirectory $unicodeInstall `
    -ArgumentList @('/S', '/PURGE-LSDJ-DATA') | Out-Null
if (-not (Test-Path -LiteralPath $unicodeApp -PathType Leaf) -or
    -not (Test-Path -LiteralPath $dataRoot -PathType Container)) {
    throw 'Unicode/space worker invocation allowed uninstall or data removal.'
}
[System.IO.File]::WriteAllText($marker, 'works.protocol.lsdj')

Invoke-CheckedProcess $unicodeUninstaller @('/S')
Start-Sleep -Milliseconds 500
if (Test-Path -LiteralPath $unicodeApp) {
    throw 'Unicode/space uninstall left the app binary behind.'
}

# The custom-location uninstall intentionally retains its remembered location.
# Override it explicitly so the final purge cleans the isolated runner's normal
# application/data root as well as the remembered-location registry state.
Start-LifecycleScenario 'final explicit cleanup after custom install location'
Invoke-CheckedProcess $newer.FullName @('/S', "/D=$dataRoot")
Invoke-CheckedProcess $uninstaller @('/S', '/PURGE-LSDJ-DATA')
Start-Sleep -Milliseconds 500
if (Test-Path -LiteralPath $dataRoot) {
    throw 'Final explicit cleanup did not remove the LSDJ data root.'
}
Require-No-Workers
Write-Host 'Windows NSIS install/upgrade/downgrade/uninstall lifecycle passed.'
