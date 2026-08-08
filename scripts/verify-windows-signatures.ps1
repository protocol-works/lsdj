[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string[]] $Path,

    [string] $ExpectedCertificateSha1 = $env:LSDJ_WINDOWS_EXPECTED_CERTIFICATE_SHA1,

    [string] $ExpectedSubject = $env:LSDJ_WINDOWS_EXPECTED_SUBJECT
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$thumbprint = $ExpectedCertificateSha1 -replace '\s', ''
if ($thumbprint -notmatch '^[0-9A-Fa-f]{40}$') {
    throw 'ExpectedCertificateSha1 must contain exactly 40 hexadecimal characters.'
}
if ([string]::IsNullOrWhiteSpace($ExpectedSubject)) {
    throw 'ExpectedSubject is required and must exactly match the approved Authenticode subject.'
}

$signTool = (Get-Command 'signtool.exe' -ErrorAction Stop).Source
foreach ($entry in $Path) {
    $target = Get-Item -LiteralPath $entry -ErrorAction Stop
    if ($target.PSIsContainer -or ($target.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Signature target must be a plain file: $entry"
    }
    if ($target.Extension -notin @('.exe', '.dll')) {
        throw "Signature target must be an executable payload: $($target.FullName)"
    }

    $signature = Get-AuthenticodeSignature -LiteralPath $target.FullName
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode signature is not valid for $($target.FullName): $($signature.StatusMessage)"
    }
    if ($null -eq $signature.SignerCertificate) {
        throw "Authenticode signer certificate is missing for $($target.FullName)."
    }
    if ($signature.SignerCertificate.Thumbprint -ne $thumbprint.ToUpperInvariant()) {
        throw "Unexpected Authenticode certificate for $($target.FullName)."
    }
    if ($signature.SignerCertificate.Subject -cne $ExpectedSubject) {
        throw "Unexpected Authenticode subject for $($target.FullName): $($signature.SignerCertificate.Subject)"
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "Authenticode timestamp is missing for $($target.FullName)."
    }

    & $signTool verify /pa /all /v $target.FullName
    if ($LASTEXITCODE -ne 0) {
        throw "signtool trust verification failed with exit code $LASTEXITCODE for $($target.FullName)."
    }
    Write-Host "Verified Authenticode signer and timestamp: $($target.FullName)"
}
