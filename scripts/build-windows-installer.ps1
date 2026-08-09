[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^v[0-9]{4}\.(0[1-9]|1[0-2])\.[1-9][0-9]*$')]
    [string] $ReleaseTag,

    [switch] $Release,

    [switch] $UnsignedDevelopment
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows -or -not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
    throw 'LSDJ Windows installers must be built by a 64-bit process on Windows x64.'
}
if ($Release -eq $UnsignedDevelopment) {
    throw 'Choose exactly one of -Release or -UnsignedDevelopment.'
}

$match = [regex]::Match($ReleaseTag, '^v(?<year>[0-9]{4})\.(?<month>[0-9]{2})\.(?<build>[1-9][0-9]*)$')
$version = '{0}.{1}.{2}' -f `
    [int] $match.Groups['year'].Value, `
    [int] $match.Groups['month'].Value, `
    [int] $match.Groups['build'].Value

$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot 'src-tauri'
$frontendDist = Join-Path $repoRoot 'frontend/dist'
if (-not (Test-Path -LiteralPath $frontendDist -PathType Container)) {
    throw 'frontend/dist is missing; build the frontend before packaging.'
}

$tempRoot = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    [System.IO.Path]::GetTempPath()
} else {
    $env:RUNNER_TEMP
}
$versionConfig = Join-Path $tempRoot "lsdj-windows-version-$([guid]::NewGuid().ToString('N')).json"
$ciInstallerHooks = $null
$versionConfiguration = @{ version = $version }
if ($UnsignedDevelopment) {
    # Hosted installer tests need one synchronization point after purge
    # confirmation and before the destructive revalidation. Compile that test
    # branch only into explicitly unsigned development installers; the release
    # config always uses the reviewed production hook directly.
    $ciInstallerHooks = Join-Path $tempRoot "lsdj-windows-hooks-$([guid]::NewGuid().ToString('N')).nsh"
    $productionHooks = Join-Path $tauriRoot 'windows/installer-hooks.nsh'
    $ciHookText = "!define LSDJ_CI_ADVERSARIAL_TESTS`r`n" +
        [System.IO.File]::ReadAllText($productionHooks)
    [System.IO.File]::WriteAllText(
        $ciInstallerHooks,
        $ciHookText,
        [System.Text.UTF8Encoding]::new($false)
    )
    $versionConfiguration['bundle'] = @{
        windows = @{
            nsis = @{ installerHooks = $ciInstallerHooks }
        }
    }
}
[System.IO.File]::WriteAllText(
    $versionConfig,
    ($versionConfiguration | ConvertTo-Json -Depth 8 -Compress),
    [System.Text.UTF8Encoding]::new($false)
)

try {
    $arguments = @(
        'tauri', 'build', '--ci', '--bundles', 'nsis',
        '--features', 'managed-backend', '--config', $versionConfig
    )
    if ($UnsignedDevelopment) {
        $arguments += '--no-sign'
    } else {
        & "$PSScriptRoot/sign-windows.ps1" -Preflight
        $arguments += @('--config', 'tauri.windows.release.conf.json')
    }

    Push-Location $tauriRoot
    try {
        & cargo @arguments
        if ($LASTEXITCODE -ne 0) {
            throw "Tauri Windows packaging failed with exit code $LASTEXITCODE."
        }
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item -LiteralPath $versionConfig -Force -ErrorAction SilentlyContinue
    if ($null -ne $ciInstallerHooks) {
        Remove-Item -LiteralPath $ciInstallerHooks -Force -ErrorAction SilentlyContinue
    }
}

$bundleRoot = Join-Path $tauriRoot 'target/release/bundle/nsis'
$matchingInstallers = @(
    Get-ChildItem -LiteralPath $bundleRoot -Filter '*-setup.exe' -File |
        Where-Object { $_.VersionInfo.ProductVersion -eq $version }
)
if ($matchingInstallers.Count -ne 1) {
    throw "Expected one Windows NSIS installer for version $version; found $($matchingInstallers.Count)."
}

$installer = $matchingInstallers[0].FullName
Write-Host "Built Windows installer: $installer"
if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_OUTPUT)) {
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "installer=$installer"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "version=$version"
}
