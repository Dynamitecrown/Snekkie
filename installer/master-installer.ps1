# Windows PowerShell 5.1 is included with supported Windows installations.
param(
    [string] $DownloadDirectory = $env:SNEKKIE_MASTER_DOWNLOAD_DIRECTORY,
    [string] $InstallDirectory = $env:SNEKKIE_MASTER_INSTALL_DIRECTORY,
    [switch] $Silent,
    [switch] $RequireSignature,
    [switch] $DownloadOnly
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Net.Http

function Get-SnekkieInstallerAsset([string] $Json) {
    $release = $Json | ConvertFrom-Json
    if ($release.draft -isnot [bool] -or $release.prerelease -isnot [bool] -or
        $release.draft -or $release.prerelease) {
        throw 'GitHub did not return a published stable Snekkie release.'
    }
    $tag = [string] $release.tag_name
    if ($tag -cnotmatch '^v?((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))$') {
        throw 'The latest release has an unexpected version tag.'
    }
    $version = $Matches[1]
    $name = "Snekkie-Setup-$version.exe"
    $assets = @($release.assets | Where-Object { $_.name -ceq $name })
    if ($assets.Count -ne 1) {
        throw 'The latest release does not contain exactly one Windows installer.'
    }
    $asset = $assets[0]
    $expectedUrl = "https://github.com/Dynamitecrown/Snekkie/releases/download/$tag/$name"
    if ([string] $asset.browser_download_url -cne $expectedUrl) {
        throw 'The installer download is not from the expected Snekkie GitHub release.'
    }
    $size = 0L
    if (-not [long]::TryParse([string] $asset.size, [ref] $size) -or $size -le 0 -or $size -gt 512MB) {
        throw 'The installer has an invalid or unexpectedly large download size.'
    }
    if ([string] $asset.digest -cnotmatch '^sha256:([0-9a-fA-F]{64})$') {
        throw 'GitHub has not supplied a valid SHA-256 digest for this installer. Please use the full installer from the release page.'
    }
    [pscustomobject] @{
        Version = $version
        Name = $name
        Url = [uri] $expectedUrl
        Size = $size
        Sha256 = $Matches[1].ToLowerInvariant()
    }
}

function Get-SnekkieHttpResponse($Client, [uri] $Uri, $CancellationToken) {
    for ($redirect = 0; $redirect -le 10; $redirect++) {
        if ($Uri.Scheme -ne 'https' -or $Uri.UserInfo) {
            throw 'The download redirected to an insecure address.'
        }
        $response = $Client.GetAsync($Uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead,
            $CancellationToken).GetAwaiter().GetResult()
        $status = [int] $response.StatusCode
        if ($status -in @(301, 302, 303, 307, 308)) {
            $location = $response.Headers.Location
            $response.Dispose()
            if ($null -eq $location) { throw 'GitHub returned a redirect without a destination.' }
            $Uri = [uri]::new($Uri, $location)
            continue
        }
        if ($status -ne 200) {
            $response.Dispose()
            if ($status -in @(403, 429)) {
                throw 'GitHub is limiting download requests. Try again later or download the full installer from the release page.'
            }
            throw "GitHub could not provide the download (HTTP $status)."
        }
        return $response
    }
    throw 'The download redirected too many times.'
}

function Get-SnekkieLatestAsset($Client) {
    $timeout = [Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds(30))
    $response = $null
    $stream = $null
    $memory = [IO.MemoryStream]::new()
    try {
        $response = Get-SnekkieHttpResponse $Client 'https://api.github.com/repos/Dynamitecrown/Snekkie/releases/latest' $timeout.Token
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $buffer = New-Object byte[] 8192
        while (($count = $stream.ReadAsync($buffer, 0, $buffer.Length, $timeout.Token).GetAwaiter().GetResult()) -gt 0) {
            if ($memory.Length + $count -gt 2MB) { throw 'GitHub returned an unexpectedly large release description.' }
            $memory.Write($buffer, 0, $count)
        }
        Get-SnekkieInstallerAsset ([Text.Encoding]::UTF8.GetString($memory.ToArray()))
    } finally {
        if ($stream) { $stream.Dispose() }
        if ($response) { $response.Dispose() }
        $memory.Dispose()
        $timeout.Dispose()
    }
}

function Save-SnekkieInstaller($Client, $Asset, [string] $Directory) {
    $null = [IO.Directory]::CreateDirectory($Directory)
    $path = Join-Path $Directory $Asset.Name
    $partial = Join-Path $Directory ("download-" + [guid]::NewGuid() + '.part')
    $timeout = [Threading.CancellationTokenSource]::new([TimeSpan]::FromMinutes(15))
    $response = $null
    $stream = $null
    $file = $null
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $response = Get-SnekkieHttpResponse $Client $Asset.Url $timeout.Token
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $file = [IO.File]::Open($partial, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $buffer = New-Object byte[] 65536
        $received = 0L
        $lastProgress = -10
        while (($count = $stream.ReadAsync($buffer, 0, $buffer.Length, $timeout.Token).GetAwaiter().GetResult()) -gt 0) {
            $received += $count
            if ($received -gt $Asset.Size) { throw 'The download exceeds the size published by GitHub.' }
            $file.Write($buffer, 0, $count)
            $null = $hasher.TransformBlock($buffer, 0, $count, $buffer, 0)
            $percent = [int] ([Math]::Floor(100.0 * $received / $Asset.Size))
            if ($percent -ge $lastProgress + 10) {
                Write-Host "Downloading Snekkie $($Asset.Version): $percent%"
                $lastProgress = $percent
            }
        }
        $file.Flush($true)
        $file.Dispose()
        $file = $null
        if ($received -ne $Asset.Size) { throw 'The download ended before the complete installer arrived.' }
        $null = $hasher.TransformFinalBlock((New-Object byte[] 0), 0, 0)
        $actual = [BitConverter]::ToString($hasher.Hash).Replace('-', '').ToLowerInvariant()
        if ($actual -cne $Asset.Sha256) { throw 'The downloaded installer does not match GitHub. Setup will not run it.' }
        [IO.File]::Move($partial, $path)
        return $path
    } finally {
        if ($file) { $file.Dispose() }
        if ($stream) { $stream.Dispose() }
        if ($response) { $response.Dispose() }
        $timeout.Dispose()
        $hasher.Dispose()
        if ([IO.File]::Exists($partial)) { [IO.File]::Delete($partial) }
    }
}

function Test-SnekkieInstallerSignature([string] $Path, [bool] $Required) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -eq 'Valid') {
        if ($null -eq $signature.TimeStamperCertificate) { throw 'The signed installer has no trusted timestamp.' }
        return
    }
    if (-not $Required -and $signature.Status -eq 'NotSigned') { return }
    throw "The installer has no acceptable digital signature ($($signature.Status)). Setup will not run it."
}

function Start-SnekkieSetup([string] $Path, [bool] $Quiet, [string] $Directory) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Path
    $start.UseShellExecute = $false
    $arguments = @()
    if ($Quiet) { $arguments += '/S' }
    if ($Directory) {
        # NSIS requires /D= to be the final argument, unquoted even with spaces.
        if ($Directory.IndexOfAny([char[]] @('"', "`r", "`n")) -ge 0 -or -not [IO.Path]::IsPathRooted($Directory)) {
            throw 'The installation directory must be an absolute path without quotes or line breaks.'
        }
        $arguments += "/D=$Directory"
    }
    $start.Arguments = $arguments -join ' '
    $process = [Diagnostics.Process]::Start($start)
    try {
        $process.WaitForExit()
        return $process.ExitCode
    } finally { $process.Dispose() }
}

function Invoke-SnekkieMasterInstaller($Client, [string] $Directory, [string] $InstallPath,
    [bool] $Quiet, [bool] $RequireSigned, [bool] $OnlyDownload) {
    if (-not $Directory) { throw 'A temporary download directory is required.' }
    Write-Host 'Checking GitHub for the latest stable Snekkie release...'
    $asset = Get-SnekkieLatestAsset $Client
    $path = Save-SnekkieInstaller $Client $asset $Directory
    $guard = $null
    $verified = $false
    try {
        # Keep the verified bytes locked against replacement until setup exits.
        $guard = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        Test-SnekkieInstallerSignature $path $RequireSigned
        $verified = $true
        Write-Host "Snekkie $($asset.Version) download verified."
        if ($OnlyDownload) {
            Write-Host "Saved installer: $path"
            return 0
        }
        Write-Host 'Opening Snekkie setup...'
        return (Start-SnekkieSetup $path $Quiet $InstallPath)
    } finally {
        if ($guard) { $guard.Dispose() }
        if ((-not $OnlyDownload -or -not $verified) -and [IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    }
}

# Dot-sourcing exposes the functions for regression checks without using the network.
if ($MyInvocation.InvocationName -ne '.') {
    $handler = $null
    $client = $null
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $handler = [Net.Http.HttpClientHandler]::new()
        $handler.AllowAutoRedirect = $false
        $client = [Net.Http.HttpClient]::new($handler)
        $client.DefaultRequestHeaders.UserAgent.ParseAdd('Snekkie-Master-Installer/1.0')
        $client.DefaultRequestHeaders.Accept.ParseAdd('application/vnd.github+json')
        $code = Invoke-SnekkieMasterInstaller $client $DownloadDirectory $InstallDirectory ([bool] $Silent) ([bool] $RequireSignature) ([bool] $DownloadOnly)
        exit $code
    } catch {
        Write-Host "Setup could not continue: $($_.Exception.Message)"
        Write-Host 'Try again, or get a full installer at https://github.com/Dynamitecrown/Snekkie/releases/latest'
        exit 2
    } finally {
        if ($client) { $client.Dispose() }
        if ($handler) { $handler.Dispose() }
    }
}
