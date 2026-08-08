[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Path
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Hosted pull-request CI deliberately creates unsigned development installers.
# Prove the release verifier refuses one with an otherwise syntactically valid
# expected identity; a zero exit would be a release-blocking fail-open bug.
$powerShell = (Get-Process -Id $PID).Path
& $powerShell -NoLogo -NoProfile -NonInteractive -File `
    "$PSScriptRoot/verify-windows-signatures.ps1" `
    -Path $Path `
    -ExpectedCertificateSha1 ('0' * 40) `
    -ExpectedSubject 'CN=Unsigned CI Sentinel'
if ($LASTEXITCODE -eq 0) {
    throw 'Release signature verification accepted an unsigned development installer.'
}
Write-Host 'Release signature verification correctly rejected the unsigned development installer.'
