[CmdletBinding()]
param(
    [string] $Path,

    [switch] $Preflight
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Require-ProtectedValue {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Name
    )

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw "Protected Windows release configuration '$Name' is missing."
    }
    return $value.Trim()
}

$providerCommand = Require-ProtectedValue 'LSDJ_WINDOWS_SIGN_COMMAND_PATH'
$expectedThumbprint = (Require-ProtectedValue 'LSDJ_WINDOWS_EXPECTED_CERTIFICATE_SHA1') -replace '\s', ''
$expectedSubject = Require-ProtectedValue 'LSDJ_WINDOWS_EXPECTED_SUBJECT'

if (-not [System.IO.Path]::IsPathFullyQualified($providerCommand)) {
    throw 'LSDJ_WINDOWS_SIGN_COMMAND_PATH must be an absolute executable path.'
}
if ($expectedThumbprint -notmatch '^[0-9A-Fa-f]{40}$') {
    throw 'LSDJ_WINDOWS_EXPECTED_CERTIFICATE_SHA1 must contain exactly 40 hexadecimal characters.'
}
$provider = Get-Item -LiteralPath $providerCommand -ErrorAction Stop
if (($provider.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $provider.PSIsContainer) {
    throw 'The Windows signing provider command must be a plain executable file, not a link or directory.'
}
(Get-Command 'signtool.exe' -ErrorAction Stop) | Out-Null

if ($Preflight) {
    Write-Host "Windows signing interface is configured for subject '$expectedSubject' and certificate $($expectedThumbprint.ToUpperInvariant())."
    exit 0
}

if ([string]::IsNullOrWhiteSpace($Path)) {
    throw 'Path is required when signing an executable payload.'
}

$target = Get-Item -LiteralPath $Path -ErrorAction Stop
if ($target.PSIsContainer -or ($target.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "Signing target must be a plain file: $Path"
}
if ($target.Extension -notin @('.exe', '.dll')) {
    throw "Refusing to sign a non-executable payload: $($target.FullName)"
}

# The selected provider owns key access and timestamp configuration. Its wrapper
# receives exactly one literal path, never a shell command string. This keeps the
# repo compatible with certificate-store, HSM, or managed/keyless providers
# without pretending one has been selected.
$global:LASTEXITCODE = 0
& $provider.FullName $target.FullName
if ($LASTEXITCODE -ne 0) {
    throw "Windows signing provider failed with exit code $LASTEXITCODE for $($target.FullName)."
}

& "$PSScriptRoot/verify-windows-signatures.ps1" `
    -Path $target.FullName `
    -ExpectedCertificateSha1 $expectedThumbprint `
    -ExpectedSubject $expectedSubject
if ($LASTEXITCODE -ne 0) {
    throw "Signature verification failed for $($target.FullName)."
}
