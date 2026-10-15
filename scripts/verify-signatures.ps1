param(
    [Parameter(Mandatory)]
    [string[]] $Path
)

$ErrorActionPreference = 'Stop'
$publisher = $null
foreach ($file in $Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file
    if ($signature.Status -ne 'Valid') {
        throw "Invalid Authenticode signature on ${file}: $($signature.Status)"
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "Missing trusted timestamp on $file"
    }
    $subject = $signature.SignerCertificate.Subject
    if ($null -ne $publisher -and $publisher -ne $subject) {
        throw "Signing publisher differs between release files: $file"
    }
    $publisher = $subject
    Write-Host "Verified signature and timestamp: $file ($subject)"
}
