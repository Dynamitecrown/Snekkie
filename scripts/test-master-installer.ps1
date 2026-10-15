# No Pester or network is required. Run with Windows PowerShell 5.1 or pwsh.
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSEdition -eq 'Core') {
    # Exercise the exact runtime that the packaged installer launches.
    & "$env:SystemRoot/System32/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath
    exit $LASTEXITCODE
}
. "$PSScriptRoot/../installer/master-installer.ps1"

if (-not ('SnekkieMasterTestHandler' -as [type])) {
    Add-Type -ReferencedAssemblies ([Net.Http.HttpClient].Assembly.Location) -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
public class SnekkieMasterTestHandler : HttpMessageHandler {
    public Queue<HttpResponseMessage> Responses = new Queue<HttpResponseMessage>();
    public List<string> Requests = new List<string>();
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) {
        Requests.Add(request.RequestUri.AbsoluteUri);
        if (Responses.Count == 0) throw new InvalidOperationException("Unexpected HTTP request");
        return Task.FromResult(Responses.Dequeue());
    }
}
'@
}

$script:Checks = 0
function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
    $script:Checks++
}
function Assert-Throws([scriptblock] $Code, [string] $Expected) {
    $message = $null
    try { & $Code | Out-Null } catch { $message = $_.Exception.Message }
    Assert-True ($null -ne $message -and $message -like $Expected) "Expected '$Expected', got '$message'"
}
function New-Response([byte[]] $Bytes, [int] $Status = 200, [string] $Location = '') {
    $statusValue = [Enum]::ToObject([Net.HttpStatusCode], $Status)
    $response = [Net.Http.HttpResponseMessage]::new($statusValue)
    $response.Content = [Net.Http.ByteArrayContent]::new($Bytes)
    if ($Location) { $response.Headers.Location = [uri] $Location }
    return $response
}
function New-FixtureRelease([byte[]] $Bytes) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $digest = [BitConverter]::ToString($sha.ComputeHash($Bytes)).Replace('-', '').ToLowerInvariant() }
    finally { $sha.Dispose() }
    return @{
        tag_name = 'v2.6.0'; draft = $false; prerelease = $false
        assets = @(@{
            name = 'Snekkie-Setup-2.6.0.exe'
            browser_download_url = 'https://github.com/Dynamitecrown/Snekkie/releases/download/v2.6.0/Snekkie-Setup-2.6.0.exe'
            size = $Bytes.Length; digest = "sha256:$digest"
        })
    }
}
function New-FixtureClient($Release, [byte[]] $Bytes, [bool] $Redirect = $false) {
    $handler = [SnekkieMasterTestHandler]::new()
    $json = $Release | ConvertTo-Json -Depth 8 -Compress
    $handler.Responses.Enqueue((New-Response ([Text.Encoding]::UTF8.GetBytes($json))))
    if ($Redirect) {
        $handler.Responses.Enqueue((New-Response (New-Object byte[] 0) 302 'https://release-assets.githubusercontent.com/fixture'))
    }
    $handler.Responses.Enqueue((New-Response $Bytes))
    return [pscustomobject] @{ Client = [Net.Http.HttpClient]::new($handler); Handler = $handler }
}

$payload = [Text.Encoding]::UTF8.GetBytes('A harmless installer fixture; never executed.')
$release = New-FixtureRelease $payload
$json = $release | ConvertTo-Json -Depth 8
$asset = Get-SnekkieInstallerAsset $json
Assert-True ($asset.Version -eq '2.6.0' -and $asset.Name -eq 'Snekkie-Setup-2.6.0.exe') 'Wrong installer selected'
$release.assets = @(@{ name='Snekkie-Setup.exe' }, $release.assets[0], @{ name='snekkie-2.6.0-portable.exe' })
Assert-True ((Get-SnekkieInstallerAsset ($release | ConvertTo-Json -Depth 8)).Name -eq $asset.Name) 'Master/portable assets must not be selected'

foreach ($property in @('draft', 'prerelease')) {
    $changed = New-FixtureRelease $payload
    $changed[$property] = $true
    Assert-Throws { Get-SnekkieInstallerAsset ($changed | ConvertTo-Json -Depth 8) } '*published stable*'
}
foreach ($tag in @('v2.6.0-beta.1', 'installer-v1.0.0', '../../evil', 'v02.6.0', 'v2.6.0.exe')) {
    $changed = New-FixtureRelease $payload
    $changed.tag_name = $tag
    Assert-Throws { Get-SnekkieInstallerAsset ($changed | ConvertTo-Json -Depth 8) } '*unexpected version*'
}
foreach ($digest in @($null, 'sha256:abc', ('sha1:' + ('a' * 64)), ('sha256:' + ('g' * 64)))) {
    $changed = New-FixtureRelease $payload
    $changed.assets[0].digest = $digest
    Assert-Throws { Get-SnekkieInstallerAsset ($changed | ConvertTo-Json -Depth 8) } '*valid SHA-256*'
}
foreach ($size in @(-1, 0, 1GB, 'not a number')) {
    $changed = New-FixtureRelease $payload
    $changed.assets[0].size = $size
    Assert-Throws { Get-SnekkieInstallerAsset ($changed | ConvertTo-Json -Depth 8) } '*download size*'
}
foreach ($url in @('http://github.com/Dynamitecrown/Snekkie/releases/download/v2.6.0/Snekkie-Setup-2.6.0.exe',
    'https://github.com/other/Snekkie/releases/download/v2.6.0/Snekkie-Setup-2.6.0.exe',
    'https://example.com/Snekkie-Setup-2.6.0.exe')) {
    $changed = New-FixtureRelease $payload
    $changed.assets[0].browser_download_url = $url
    Assert-Throws { Get-SnekkieInstallerAsset ($changed | ConvertTo-Json -Depth 8) } '*expected Snekkie GitHub*'
}
$changed = New-FixtureRelease $payload
$changed.assets += $changed.assets[0]
Assert-Throws { Get-SnekkieInstallerAsset ($changed | ConvertTo-Json -Depth 8) } '*exactly one*'
$changed.assets = @()
Assert-Throws { Get-SnekkieInstallerAsset ($changed | ConvertTo-Json -Depth 8) } '*exactly one*'

# Substitute signature inspection and setup launch for synthetic download fixtures.
# No downloaded file is executed by these tests.
$script:SignatureStatus = 'NotSigned'
$script:HasTimestamp = $true
$script:SetupCalls = 0
$script:SetupExit = 0
$realStartSetup = (Get-Command Start-SnekkieSetup).ScriptBlock
function Get-AuthenticodeSignature {
    param([string] $LiteralPath)
    [pscustomobject] @{
        Status = $script:SignatureStatus
        TimeStamperCertificate = $(if ($script:HasTimestamp) { 'fixture timestamp' } else { $null })
    }
}
function Start-SnekkieSetup([string] $Path, [bool] $Quiet, [string] $Directory) {
    $script:SetupCalls++
    Assert-True ([IO.File]::ReadAllText($Path) -eq [Text.Encoding]::UTF8.GetString($payload)) 'Setup received altered bytes'
    Assert-True ($Quiet -and $Directory -eq 'C:\Fixture install with spaces\') 'Silent mode or directory was lost'
    Assert-Throws { [IO.File]::WriteAllText($Path, 'replace verified download') } '*'
    return $script:SetupExit
}

$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('snekkie-master-tests-' + [guid]::NewGuid())
$null = [IO.Directory]::CreateDirectory($testRoot)
try {
    # Exercise the real child-process adapter with an inert local fixture.
    $stubPath = Join-Path $testRoot 'setup fixture.exe'
    $env:SNEKKIE_MASTER_TEST_ARGUMENTS = Join-Path $testRoot 'arguments.txt'
    Add-Type -OutputAssembly $stubPath -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
public class SnekkieSetupProcessFixture {
    public static int Main(string[] args) {
        File.WriteAllText(Environment.GetEnvironmentVariable("SNEKKIE_MASTER_TEST_ARGUMENTS"), Environment.CommandLine);
        return 5;
    }
}
'@
    $childCode = & $realStartSetup $stubPath $true 'C:\Fixture install with spaces\'
    Assert-True ($childCode -eq 5) 'The real process adapter lost the child exit code'
    $commandLine = Get-Content -LiteralPath $env:SNEKKIE_MASTER_TEST_ARGUMENTS -Raw
    Assert-True ($commandLine.EndsWith(' /S /D=C:\Fixture install with spaces\')) 'Real silent/directory arguments were not forwarded intact'
    Assert-Throws { & $realStartSetup $stubPath $true 'relative-directory' } '*absolute path*'
    Assert-Throws { & $realStartSetup $stubPath $true 'C:\bad"directory' } '*without quotes*'

    foreach ($exitCode in @(0, 1, 5)) {
        $fixture = New-FixtureClient (New-FixtureRelease $payload) $payload $true
        $directory = Join-Path $testRoot ('success-' + $exitCode)
        $script:SetupExit = $exitCode
        try {
            $actual = Invoke-SnekkieMasterInstaller $fixture.Client $directory 'C:\Fixture install with spaces\' $true $false $false
            Assert-True ($actual -eq $exitCode) 'Child installer exit status was not preserved'
            Assert-True ($fixture.Handler.Requests.Count -eq 3) 'HTTPS redirect was not followed'
            Assert-True (@(Get-ChildItem $directory).Count -eq 0) 'Downloaded installer was not cleaned up'
        } finally { $fixture.Client.Dispose() }
    }
    Assert-True ($script:SetupCalls -eq 3) 'Expected three verified setup invocations'

    foreach ($kind in @('short', 'long', 'hash', 'unsigned-required', 'bad-signature', 'missing-timestamp')) {
        $metadata = New-FixtureRelease $payload
        $bytes = $payload
        $required = $false
        $script:SignatureStatus = 'NotSigned'
        $script:HasTimestamp = $true
        switch ($kind) {
            'short' { $bytes = $payload[0..($payload.Length - 2)]; $expected = '*complete installer*' }
            'long' { $bytes = $payload + [byte] 1; $expected = '*exceeds the size*' }
            'hash' { $metadata.assets[0].digest = 'sha256:' + ('0' * 64); $expected = '*does not match GitHub*' }
            'unsigned-required' { $required = $true; $expected = '*acceptable digital signature*' }
            'bad-signature' { $script:SignatureStatus = 'HashMismatch'; $expected = '*acceptable digital signature*' }
            'missing-timestamp' { $script:SignatureStatus = 'Valid'; $script:HasTimestamp = $false; $expected = '*trusted timestamp*' }
        }
        $fixture = New-FixtureClient $metadata $bytes
        $directory = Join-Path $testRoot $kind
        try {
            Assert-Throws { Invoke-SnekkieMasterInstaller $fixture.Client $directory '' $false $required $false } $expected
            Assert-True (@(Get-ChildItem $directory).Count -eq 0) "Failed download was not removed: $kind"
            Assert-True ($script:SetupCalls -eq 3) "Unverified setup was launched: $kind"
        } finally { $fixture.Client.Dispose() }
    }
    $script:SignatureStatus = 'Valid'
    $script:HasTimestamp = $true
    $fixture = New-FixtureClient (New-FixtureRelease $payload) $payload
    try {
        $directory = Join-Path $testRoot 'download-only'
        $actual = Invoke-SnekkieMasterInstaller $fixture.Client $directory '' $false $true $true
        Assert-True ($actual -eq 0 -and (Test-Path "$directory/Snekkie-Setup-2.6.0.exe")) 'Verified download-only file was not retained'
        Assert-True ($script:SetupCalls -eq 3) 'Download-only launched setup'
    } finally { $fixture.Client.Dispose() }

    foreach ($status in @(404, 403, 429, 500)) {
        $handler = [SnekkieMasterTestHandler]::new()
        $handler.Responses.Enqueue((New-Response (New-Object byte[] 0) $status))
        $client = [Net.Http.HttpClient]::new($handler)
        try { Assert-Throws { Get-SnekkieLatestAsset $client } '*GitHub*' }
        finally { $client.Dispose() }
    }
    $handler = [SnekkieMasterTestHandler]::new()
    $handler.Responses.Enqueue((New-Response (New-Object byte[] 0) 302 'http://example.com/download'))
    $client = [Net.Http.HttpClient]::new($handler)
    $timeout = [Threading.CancellationTokenSource]::new()
    try {
        Assert-Throws { Get-SnekkieHttpResponse $client 'https://github.com/start' $timeout.Token } '*insecure address*'
        Assert-True ($handler.Requests.Count -eq 1) 'An HTTP downgrade was requested'
    } finally { $client.Dispose(); $timeout.Dispose() }
} finally {
    # This exact directory was created for these fixtures under the OS temp folder.
    $resolved = [IO.Path]::GetFullPath($testRoot)
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolved.StartsWith($temp, [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path $resolved -Leaf) -notlike 'snekkie-master-tests-*') { throw 'Unsafe test cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
Write-Host "PASS: $script:Checks master installer regression checks"
