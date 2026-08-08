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
# expected identity. The exact NotSigned failure is required so a missing SDK
# tool or unrelated script error cannot masquerade as successful rejection.
$powerShell = (Get-Process -Id $PID).Path
$PSNativeCommandUseErrorActionPreference = $false
$output = & $powerShell -NoLogo -NoProfile -NonInteractive -File `
    "$PSScriptRoot/verify-windows-signatures.ps1" `
    -Path $Path `
    -ExpectedCertificateSha1 ('0' * 40) `
    -ExpectedSubject 'CN=Unsigned CI Sentinel' 2>&1
$exitCode = $LASTEXITCODE
$rendered = $output | Out-String
if ($exitCode -eq 0) {
    throw 'Release signature verification accepted an unsigned development installer.'
}
if ($rendered -notmatch 'Authenticode signature status is NotSigned') {
    throw "Release verification failed for an unexpected reason instead of rejecting an unsigned artifact:`n$rendered"
}
Write-Host 'Release signature verification correctly rejected the unsigned development installer.'
