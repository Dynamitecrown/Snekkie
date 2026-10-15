param(
    [string] $Output = 'dist/Snekkie-Setup.exe',
    [switch] $RequireSignedRelease
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content -LiteralPath "$root/installer/master-version.txt" -Raw).Trim()
if ($version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw 'Invalid master installer version.'
}
$compiler = (Get-Command makensis -ErrorAction SilentlyContinue).Source
if (-not $compiler) { $compiler = "${env:ProgramFiles(x86)}/NSIS/makensis.exe" }
if (-not (Test-Path -LiteralPath $compiler)) { throw 'Install NSIS 3 to build the master installer.' }
if (-not [IO.Path]::IsPathRooted($Output)) { $Output = Join-Path $root $Output }
$Output = [IO.Path]::GetFullPath($Output)
$null = [IO.Directory]::CreateDirectory((Split-Path $Output -Parent))
$arguments = @('/V2', "/DVERSION=$version", "/DOUTFILE=$Output")
if ($RequireSignedRelease) { $arguments += '/DREQUIRE_SIGNED_RELEASE' }
$arguments += "$root/installer/master.nsi"
& $compiler @arguments
if ($LASTEXITCODE -ne 0) { throw "NSIS failed (exit $LASTEXITCODE)." }
Write-Host "Built $Output (master version $version)"
