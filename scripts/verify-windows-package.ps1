param(
    [Parameter(Mandatory)]
    [string] $Version,
    [Parameter(Mandatory)]
    [string] $DistDirectory,
    [Parameter(Mandatory)]
    [string] $PayloadDirectory,
    [switch] $RequireSignatures
)

$ErrorActionPreference = 'Stop'
$installer = Join-Path $DistDirectory "Snekkie-Setup-$Version.exe"
$portable = Join-Path $DistDirectory "snekkie-$Version-portable.exe"
$app = Join-Path $PayloadDirectory 'snekkie.exe'
$uninstaller = Join-Path $PayloadDirectory 'uninstall.exe'

function Assert-SameFile([string] $Actual, [string] $Expected) {
    if ((Get-FileHash -LiteralPath $Actual -Algorithm SHA256).Hash -ne
        (Get-FileHash -LiteralPath $Expected -Algorithm SHA256).Hash) {
        throw "Packaged file differs from verified payload: $Actual"
    }
}

Assert-SameFile $portable $app
if ($RequireSignatures) {
    & "$PSScriptRoot/verify-signatures.ps1" -Path @($installer, $portable, $app, $uninstaller)
}

# Extract without running setup or changing the runner's installation/registry.
$extractDirectory = Join-Path ([IO.Path]::GetTempPath()) ("snekkie-package-" + [guid]::NewGuid())
$sevenZip = (Get-Command 7z -ErrorAction SilentlyContinue).Source
if (-not $sevenZip) {
    $sevenZip = Join-Path $env:ProgramFiles '7-Zip/7z.exe'
}
if (-not (Test-Path -LiteralPath $sevenZip)) {
    throw '7-Zip is required to verify the embedded installer payload.'
}
& $sevenZip x $installer "-o$extractDirectory" -y -bso0 -bsp0
if ($LASTEXITCODE -ne 0) {
    throw "7-Zip could not extract the installer (exit $LASTEXITCODE)."
}
Assert-SameFile (Join-Path $extractDirectory 'snekkie.exe') $app
Assert-SameFile (Join-Path $extractDirectory 'uninstall.exe') $uninstaller
Write-Host 'Verified portable EXE and both embedded installer executables.'
