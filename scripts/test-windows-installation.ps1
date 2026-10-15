# Real installation tests. Only run on a disposable GitHub-hosted Windows VM.
# This deliberately refuses local and self-hosted runners, even with -WhatIf.
param(
    [Parameter(Mandatory)]
    [string] $MasterInstaller
)

$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_OS -cne 'Windows' -or
    $env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or $env:OS -ne 'Windows_NT') {
    throw 'Installation tests require a disposable GitHub-hosted Windows runner. Do not run locally or on a self-hosted runner.'
}

$masterPath = (Resolve-Path -LiteralPath $MasterInstaller).ProviderPath
if (-not (Test-Path -LiteralPath $masterPath -PathType Leaf) -or
    [IO.Path]::GetExtension($masterPath) -ine '.exe') {
    throw 'MasterInstaller must be an existing built Windows installer EXE.'
}
if (-not $env:RUNNER_TEMP -or -not $env:APPDATA -or -not $env:LOCALAPPDATA) {
    throw 'The runner must supply RUNNER_TEMP, APPDATA and LOCALAPPDATA.'
}

$appRegistry = 'HKCU:\Software\Snekkie'
$uninstallRegistry = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Snekkie'
$registryPaths = @($appRegistry, $uninstallRegistry,
    'HKCU:\Software\Wow6432Node\Snekkie',
    'HKCU:\Software\Wow6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Snekkie')
$dataDirectory = Join-Path $env:APPDATA 'snekkie'
$startShortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Snekkie.lnk'
$desktopShortcut = Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) 'Snekkie.lnk'
$defaultInstall = Join-Path $env:LOCALAPPDATA 'Programs\Snekkie'

# Refuse any existing user state before creating directories, logs, or processes.
# Legacy data is checked because launching Snekkie would migrate it automatically.
foreach ($path in @($registryPaths) + @($dataDirectory, $startShortcut,
        $desktopShortcut, $defaultInstall, (Join-Path $env:APPDATA 'pyterm'))) {
    if (Test-Path -LiteralPath $path) { throw "The runner is not clean; refusing to touch existing state: $path" }
}
if (@(Get-Process -Name snekkie -ErrorAction SilentlyContinue).Count -gt 0) {
    throw 'A Snekkie process already exists; refusing to run installation tests.'
}
$existingMutex = $null
if ([Threading.Mutex]::TryOpenExisting('Snekkie.Running', [ref] $existingMutex)) {
    $existingMutex.Dispose()
    throw 'The Snekkie.Running mutex already exists; refusing to run installation tests.'
}

. "$PSScriptRoot/../installer/master-installer.ps1"

$script:checks = 0
$script:ownedProcesses = New-Object 'System.Collections.Generic.List[object]'
$testRoot = [IO.Path]::GetFullPath((Join-Path $env:RUNNER_TEMP ('snekkie-windows-e2e-' + [guid]::NewGuid())))
$runnerTemp = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\') + '\'
if (-not $testRoot.StartsWith($runnerTemp, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The test directory must be a child of RUNNER_TEMP.'
}
$logDirectory = Join-Path $testRoot 'logs'
$freshInstall = Join-Path $testRoot 'Fresh install with spaces'
$upgradeInstall = Join-Path $testRoot 'Older install with spaces'
$conflictingInstall = Join-Path $testRoot 'Must not install here'
$cancelledInstall = Join-Path $testRoot 'Cancelled install with spaces'
$cancellationFixture = Join-Path $testRoot 'native-cancellation-fixture'
$settingsPath = Join-Path $dataDirectory 'settings.json'
$networkPath = Join-Path $dataDirectory 'network.ini'
$sessionPath = Join-Path $dataDirectory 'sessions.json'
$script:ownedInstallDirectories = @($freshInstall, $upgradeInstall, $conflictingInstall, $cancelledInstall)
$mutex = $null
$handler = $null
$client = $null
$shell = $null
$transcriptStarted = $false
$failure = $null
$cleanupErrors = New-Object 'System.Collections.Generic.List[string]'

function Assert-Installation([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
    $script:checks++
    Write-Host "PASS: $Message"
}

function Wait-Installation([scriptblock] $Condition, [string] $Message, [int] $Seconds = 45) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out: $Message"
}

function Start-TestInstaller([string] $Path, [string] $Arguments, [string] $Label) {
    # NSIS /D= is deliberately last and unquoted, including paths with spaces.
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Path
    $start.Arguments = $Arguments
    $start.UseShellExecute = $false
    $start.WorkingDirectory = $testRoot
    Write-Host "Starting $Label`: $Path $Arguments"
    $process = [Diagnostics.Process]::Start($start)
    $script:ownedProcesses.Add([pscustomobject] @{ Process = $process; Path = $Path; Label = $Label })
    return $process
}

function Complete-TestInstaller($Process, [string] $Label, [int] $ExpectedExit = 0) {
    if (-not $Process.WaitForExit(900000)) { throw "$Label did not finish within 15 minutes." }
    Assert-Installation ($Process.ExitCode -eq $ExpectedExit) "$Label exited $ExpectedExit (actual $($Process.ExitCode))"
}

function Assert-Installed([string] $Directory, [string] $Version) {
    $exe = Join-Path $Directory 'snekkie.exe'
    Assert-Installation (Test-Path -LiteralPath $exe -PathType Leaf) 'Installed executable exists'
    Assert-Installation (Test-Path -LiteralPath (Join-Path $Directory 'uninstall.exe') -PathType Leaf) 'Uninstaller exists'
    $exeVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion
    Assert-Installation ($exeVersion -match ('^' + [regex]::Escape($Version) + '(?:\.0)?$')) "Executable product version is $Version (actual $exeVersion)"
    $registration = Get-ItemProperty -LiteralPath $appRegistry
    $uninstall = Get-ItemProperty -LiteralPath $uninstallRegistry
    Assert-Installation ($registration.InstallDir -ieq $Directory -and $registration.Version -eq $Version) 'Application registration uses the installed directory and version'
    Assert-Installation ($uninstall.InstallLocation -ieq $Directory -and $uninstall.DisplayVersion -eq $Version) 'Apps and features registration uses the installed directory and version'
    Assert-Installation ($uninstall.QuietUninstallString -eq ('"' + (Join-Path $Directory 'uninstall.exe') + '" /S')) 'Quiet uninstall command targets this installation'
    Assert-Shortcut $startShortcut $exe
}

function Assert-Shortcut([string] $Path, [string] $Target) {
    Assert-Installation (Test-Path -LiteralPath $Path -PathType Leaf) "Shortcut exists: $Path"
    $shortcut = $shell.CreateShortcut($Path)
    try { Assert-Installation ($shortcut.TargetPath -ieq $Target) "Shortcut targets the installed executable: $Path" }
    finally { [void] [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) }
}

function Save-SyntheticUserData([switch] $Online) {
    $null = [IO.Directory]::CreateDirectory($dataDirectory)
    $settings = if ($Online) {
        '{"theme":"Solarized Dark","font_size":17,"check_for_updates":true,"offline_mode":false}'
    } else {
        '{"theme":"Solarized Dark","font_size":17,"check_for_updates":false,"offline_mode":true}'
    }
    [IO.File]::WriteAllText($settingsPath, $settings)
    [IO.File]::WriteAllText($sessionPath, '[]')
    # Include an extra section so byte preservation proves this was not rewritten.
    $offlineValue = if ($Online) { 0 } else { 1 }
    $updateValue = if ($Online) { 1 } else { 0 }
    [IO.File]::WriteAllText($networkPath, "[Network]`r`nOfflineMode=$offlineValue`r`nCheckForUpdates=$updateValue`r`n`r`n[TestMarker]`r`nValue=preserve-this-policy`r`n")
    return @{
        Settings = (Get-FileHash -LiteralPath $settingsPath -Algorithm SHA256).Hash
        Sessions = (Get-FileHash -LiteralPath $sessionPath -Algorithm SHA256).Hash
        Network = (Get-FileHash -LiteralPath $networkPath -Algorithm SHA256).Hash
    }
}

function Assert-UserData($Expected) {
    foreach ($pair in @(@($settingsPath, $Expected.Settings), @($sessionPath, $Expected.Sessions), @($networkPath, $Expected.Network))) {
        Assert-Installation ((Get-FileHash -LiteralPath $pair[0] -Algorithm SHA256).Hash -eq $pair[1]) "User file preserved byte for byte: $($pair[0])"
    }
}

function Uninstall-TestInstallation([string] $Directory) {
    $process = Start-TestInstaller (Join-Path $Directory 'uninstall.exe') '/S' 'Silent uninstall'
    Complete-TestInstaller $process 'Silent uninstall'
    # NSIS launches a temporary copy of its uninstaller, so wait for the effects too.
    Wait-Installation { -not (Test-Path -LiteralPath $appRegistry) -and
        -not (Test-Path -LiteralPath $uninstallRegistry) -and
        -not (Test-Path -LiteralPath (Join-Path $Directory 'snekkie.exe')) -and
        -not (Test-Path -LiteralPath $startShortcut) -and
        -not (Test-Path -LiteralPath $desktopShortcut) } 'Uninstall to remove executable, registry keys, and shortcuts'
    Assert-Installation (-not (Test-Path -LiteralPath $appRegistry) -and -not (Test-Path -LiteralPath $uninstallRegistry)) 'Uninstall removed both registration keys'
    Assert-Installation (-not (Test-Path -LiteralPath (Join-Path $Directory 'snekkie.exe'))) 'Uninstall removed the executable'
    Assert-Installation (-not (Test-Path -LiteralPath $startShortcut) -and -not (Test-Path -LiteralPath $desktopShortcut)) 'Uninstall removed both shortcuts'
}

function Get-OldInstallerAsset {
    $timeout = [Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds(30))
    $response = $null
    try {
        $response = Get-SnekkieHttpResponse $client 'https://api.github.com/repos/Dynamitecrown/Snekkie/releases/tags/v2.5.0' $timeout.Token
        $json = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
        if ([Text.Encoding]::UTF8.GetByteCount($json) -gt 2MB) { throw 'Old release metadata exceeds 2 MB.' }
        # Reuse the master parser: exact repository/tag/asset URL, size, digest,
        # stable status, and version must all pass before downloading executable code.
        $asset = Get-SnekkieInstallerAsset $json
        Assert-Installation ($asset.Version -eq '2.5.0') 'Old fixture is exactly the published stable v2.5.0 installer'
        return $asset
    } finally {
        if ($response) { $response.Dispose() }
        $timeout.Dispose()
    }
}

function Get-TestAppProcesses([string] $Path) {
    # Select by executable path, never by process name alone when stopping apps.
    return @(Get-CimInstance Win32_Process -Filter "Name='snekkie.exe'" |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath -ieq $Path })
}

function Stop-TestApp($App) {
    $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($App.ProcessId)" -ErrorAction SilentlyContinue
    if ($current -and $current.ExecutablePath -ieq $App.ExecutablePath -and
        $current.CreationDate -eq $App.CreationDate) {
        Stop-Process -Id $App.ProcessId -Force
        Wait-Installation { -not (Get-Process -Id $App.ProcessId -ErrorAction SilentlyContinue) } 'Test app to exit' 15
    }
}

try {
    $null = [IO.Directory]::CreateDirectory($logDirectory)
    Start-Transcript -LiteralPath (Join-Path $logDirectory 'installation.log') | Out-Null
    $transcriptStarted = $true
    Write-Host "Isolated installer test root: $testRoot"
    Write-Host "Master SHA-256: $((Get-FileHash -LiteralPath $masterPath -Algorithm SHA256).Hash)"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('Snekkie-Windows-Installation-Tests/1.0')
    $client.DefaultRequestHeaders.Accept.ParseAdd('application/vnd.github+json')
    $shell = New-Object -ComObject WScript.Shell

    # Build the exact native wrapper around a harmless exit-only PowerShell
    # payload. This checks cancellation propagation through real nsExec and
    # PowerShell processes without downloading or installing any app files.
    $fixtureInstaller = Join-Path $cancellationFixture 'installer'
    $fixtureAssets = Join-Path $cancellationFixture 'assets'
    $null = [IO.Directory]::CreateDirectory($fixtureInstaller)
    $null = [IO.Directory]::CreateDirectory($fixtureAssets)
    Copy-Item -LiteralPath "$PSScriptRoot/../installer/master.nsi" -Destination (Join-Path $fixtureInstaller 'master.nsi')
    Copy-Item -LiteralPath "$PSScriptRoot/../installer/welcome.bmp" -Destination (Join-Path $fixtureInstaller 'welcome.bmp')
    Copy-Item -LiteralPath "$PSScriptRoot/../assets/icon.ico" -Destination (Join-Path $fixtureAssets 'icon.ico')
    [IO.File]::WriteAllText((Join-Path $fixtureInstaller 'master-installer.ps1'), "exit 1`r`n")
    $masterVersion = (Get-Content -LiteralPath "$PSScriptRoot/../installer/master-version.txt" -Raw).Trim()
    if ($masterVersion -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw 'Invalid master installer version for cancellation fixture.'
    }
    $compiler = (Get-Command makensis -ErrorAction SilentlyContinue).Source
    if (-not $compiler) { $compiler = "${env:ProgramFiles(x86)}/NSIS/makensis.exe" }
    if (-not (Test-Path -LiteralPath $compiler -PathType Leaf)) { throw 'NSIS is required to test native wrapper cancellation.' }
    $fixtureExe = Join-Path $cancellationFixture 'cancelled-master.exe'
    & $compiler '/V2' "/DVERSION=$masterVersion" "/DOUTFILE=$fixtureExe" (Join-Path $fixtureInstaller 'master.nsi')
    Assert-Installation ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $fixtureExe -PathType Leaf)) 'Built the production native wrapper with a cancellation-only payload'
    $process = Start-TestInstaller $fixtureExe "/S /D=$cancelledInstall" 'Native master cancellation'
    Complete-TestInstaller $process 'Native master cancellation' 1
    foreach ($path in @($registryPaths) + @($cancelledInstall, $defaultInstall,
            $dataDirectory, $startShortcut, $desktopShortcut)) {
        Assert-Installation (-not (Test-Path -LiteralPath $path)) "Cancellation left no installation or user state: $path"
    }

    $latest = Get-SnekkieLatestAsset $client
    Assert-Installation ([version] $latest.Version -gt [version] '2.5.0') 'Latest stable app is newer than the upgrade fixture'
    Write-Host "Testing latest stable app $($latest.Version)"
    $latestInstaller = Save-SnekkieInstaller $client $latest (Join-Path $testRoot 'latest-download')
    Test-SnekkieInstallerSignature $latestInstaller $false
    $oldAsset = Get-OldInstallerAsset
    $oldInstaller = Save-SnekkieInstaller $client $oldAsset (Join-Path $testRoot 'old-download')
    Test-SnekkieInstallerSignature $oldInstaller $false

    # Exercise the packaged master, its PowerShell runtime, download validation,
    # forwarding of /S, and unquoted /D= with spaces in a real first install.
    $process = Start-TestInstaller $masterPath "/S /D=$freshInstall" 'Master fresh installation'
    Complete-TestInstaller $process 'Master fresh installation'
    Assert-Installed $freshInstall $latest.Version
    Assert-Installation (-not (Test-Path -LiteralPath $defaultInstall)) 'Fresh master install honored the custom path'
    $latestExeHash = (Get-FileHash -LiteralPath (Join-Path $freshInstall 'snekkie.exe') -Algorithm SHA256).Hash
    $data = Save-SyntheticUserData
    Uninstall-TestInstallation $freshInstall
    Assert-UserData $data

    $process = Start-TestInstaller $oldInstaller "/S /D=$upgradeInstall" 'Install old stable version'
    Complete-TestInstaller $process 'Install old stable version'
    Assert-Installed $upgradeInstall '2.5.0'
    $data = Save-SyntheticUserData
    # Silent setup creates Start menu only; emulate a user-created desktop link.
    $shortcut = $shell.CreateShortcut($desktopShortcut)
    $shortcut.TargetPath = Join-Path $upgradeInstall 'snekkie.exe'
    $shortcut.Save()
    [void] [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut)
    Assert-Shortcut $desktopShortcut (Join-Path $upgradeInstall 'snekkie.exe')

    $process = Start-TestInstaller $masterPath "/S /D=$conflictingInstall" 'Master upgrade with conflicting directory'
    Complete-TestInstaller $process 'Master upgrade with conflicting directory'
    Assert-Installed $upgradeInstall $latest.Version
    Assert-Installation (-not (Test-Path -LiteralPath $conflictingInstall)) 'Upgrade ignored conflicting /D and kept the registered path'
    Assert-UserData $data
    Assert-Shortcut $desktopShortcut (Join-Path $upgradeInstall 'snekkie.exe')
    $upgradeExe = Join-Path $upgradeInstall 'snekkie.exe'
    Assert-Installation ((Get-FileHash -LiteralPath $upgradeExe -Algorithm SHA256).Hash -eq $latestExeHash) 'Master upgrade installed the same latest executable as fresh setup'

    $created = $false
    $mutex = [Threading.Mutex]::new($false, 'Snekkie.Running', [ref] $created)
    Assert-Installation $created 'Test owns the running-app mutex without an existing app'
    $process = Start-TestInstaller $latestInstaller '/S' 'Running-app refusal'
    Complete-TestInstaller $process 'Running-app refusal' 5
    Assert-Installation ((Get-FileHash -LiteralPath $upgradeExe -Algorithm SHA256).Hash -eq $latestExeHash) 'Running-app refusal did not replace the executable'
    Assert-Installed $upgradeInstall $latest.Version
    Assert-UserData $data

    $process = Start-TestInstaller $latestInstaller '/UPDATE /S' 'Update wait and relaunch'
    Start-Sleep -Seconds 2
    Assert-Installation (-not $process.HasExited) '/UPDATE waits while the running-app mutex exists'
    Assert-Installation ((Get-FileHash -LiteralPath $upgradeExe -Algorithm SHA256).Hash -eq $latestExeHash) 'Waiting update leaves the executable unchanged'
    $mutex.Dispose()
    $mutex = $null
    Complete-TestInstaller $process 'Update wait and relaunch'
    Wait-Installation { @(Get-TestAppProcesses $upgradeExe).Count -eq 1 } 'Updated app to relaunch' 30
    $relaunched = @(Get-TestAppProcesses $upgradeExe)
    Assert-Installation ($relaunched.Count -eq 1) '/UPDATE relaunched exactly one app from the registered path'
    Start-Sleep -Seconds 2
    Assert-Installation (@(Get-TestAppProcesses $upgradeExe).Count -eq 1) 'Relaunched app remains running'
    Assert-Installed $upgradeInstall $latest.Version
    Assert-UserData $data
    Stop-TestApp $relaunched[0]

    Uninstall-TestInstallation $upgradeInstall
    Assert-UserData $data

    # Exercise the production menu, download worker, /UPDATE invocation and Close
    # viewport command using the opt-in UI harness, against a real older install.
    # Its own app settings live in a temporary directory; the installed app keeps
    # the fixtures below and its real /UPDATE relaunch uses the registered path.
    $process = Start-TestInstaller $oldInstaller "/S /D=$upgradeInstall" 'Reinstall old version for Update button'
    Complete-TestInstaller $process 'Reinstall old version for Update button'
    Assert-Installed $upgradeInstall '2.5.0'
    $data = Save-SyntheticUserData -Online
    $shortcut = $shell.CreateShortcut($desktopShortcut)
    $shortcut.TargetPath = Join-Path $upgradeInstall 'snekkie.exe'
    $shortcut.Save()
    [void] [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut)
    $oldUiInstall = $env:SNEKKIE_WINDOWS_UPDATE_E2E_INSTALL
    try {
        $env:SNEKKIE_WINDOWS_UPDATE_E2E_INSTALL = $upgradeInstall
        Push-Location (Join-Path $PSScriptRoot '..')
        try {
            & cargo test --locked --test windows_update -- --ignored --nocapture --test-threads=1
            $cargoExit = $LASTEXITCODE
            Assert-Installation ($cargoExit -eq 0) 'Production Update button downloaded, launched setup, requested app close, and restarted the upgraded app'
        } finally { Pop-Location }
    } finally { $env:SNEKKIE_WINDOWS_UPDATE_E2E_INSTALL = $oldUiInstall }
    Assert-Installed $upgradeInstall $latest.Version
    Assert-UserData $data
    Assert-Shortcut $desktopShortcut $upgradeExe
    Assert-Installation ((Get-FileHash -LiteralPath $upgradeExe -Algorithm SHA256).Hash -eq $latestExeHash) 'Update button installed the same latest executable as the master'
    $relaunched = @(Get-TestAppProcesses $upgradeExe)
    Assert-Installation ($relaunched.Count -eq 1) 'Update button left exactly one restarted app at the registered path'
    Stop-TestApp $relaunched[0]
    Uninstall-TestInstallation $upgradeInstall
    Assert-UserData $data
    Write-Host "Windows installation integration checks passed: $script:checks"
} catch {
    $failure = $_
    Write-Host "Windows installation integration check failed: $($_.Exception.Message)"
} finally {
    if ($mutex) { $mutex.Dispose() }
    # Kill only process handles launched here or exact path/PID/creation-time app
    # identities. Never stop unrelated applications or use name-wide Stop-Process.
    foreach ($owned in $script:ownedProcesses) {
        try {
            if (-not $owned.Process.HasExited) {
                Write-Host "Stopping unfinished test process: $($owned.Label) (PID $($owned.Process.Id))"
                # Include only descendants of this still-live owned process:
                # the master's downloader and NSIS child must stop as well.
                & "$env:SystemRoot\System32\taskkill.exe" /PID $owned.Process.Id /T /F
                $null = $owned.Process.WaitForExit(10000)
            }
        } catch { $cleanupErrors.Add("Process cleanup: $($_.Exception.Message)") }
    }
    # Also catch an app that launched immediately before an assertion failed.
    foreach ($directory in @($freshInstall, $upgradeInstall)) {
        try {
            foreach ($app in @(Get-TestAppProcesses (Join-Path $directory 'snekkie.exe'))) { Stop-TestApp $app }
        } catch { $cleanupErrors.Add("App cleanup: $($_.Exception.Message)") }
    }
    foreach ($key in @($uninstallRegistry, $appRegistry)) {
        try {
            if (Test-Path -LiteralPath $key) {
                $value = Get-ItemProperty -LiteralPath $key
                $registeredDirectory = if ($key -eq $appRegistry) { $value.InstallDir } else { $value.InstallLocation }
                if ($registeredDirectory -and $registeredDirectory -in $script:ownedInstallDirectories) {
                    Remove-Item -LiteralPath $key -Recurse -Force
                } else { throw "Refusing to remove registry key whose directory is not test-owned: $key" }
            }
        } catch { $cleanupErrors.Add("Registry cleanup: $($_.Exception.Message)") }
    }
    foreach ($path in @($startShortcut, $desktopShortcut)) {
        try {
            if (Test-Path -LiteralPath $path) {
                $shortcut = $shell.CreateShortcut($path)
                $target = $shortcut.TargetPath
                [void] [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut)
                if ($target -notin @((Join-Path $freshInstall 'snekkie.exe'), (Join-Path $upgradeInstall 'snekkie.exe'))) {
                    throw "Refusing to delete shortcut with a non-test target: $path"
                }
                Remove-Item -LiteralPath $path -Force
            }
        } catch { $cleanupErrors.Add("Shortcut cleanup: $($_.Exception.Message)") }
    }
    # All of these were absent in the clean-profile preflight. Resolve absolute
    # targets and reject junctions before any recursive removal.
    foreach ($path in @($dataDirectory, $freshInstall, $upgradeInstall, $conflictingInstall,
            $cancelledInstall, $cancellationFixture,
            (Join-Path $testRoot 'latest-download'), (Join-Path $testRoot 'old-download'))) {
        try {
            if (Test-Path -LiteralPath $path) {
                $resolved = (Resolve-Path -LiteralPath $path).ProviderPath.TrimEnd('\')
                $expected = [IO.Path]::GetFullPath($path).TrimEnd('\')
                if ($resolved -ine $expected -or
                    ($expected -ine [IO.Path]::GetFullPath($dataDirectory) -and
                        -not $expected.StartsWith($testRoot + '\', [StringComparison]::OrdinalIgnoreCase))) {
                    throw "Refusing recursive deletion outside an exact test-owned directory: $path"
                }
                $reparse = @(Get-Item -LiteralPath $path) + @(Get-ChildItem -LiteralPath $path -Force -Recurse)
                if (@($reparse | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count -gt 0) {
                    throw "Refusing recursive deletion of a directory containing a reparse point: $path"
                }
                Remove-Item -LiteralPath $resolved -Recurse -Force
            }
        } catch { $cleanupErrors.Add("Directory cleanup: $($_.Exception.Message)") }
    }
    if ($shell) { [void] [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell) }
    if ($client) { $client.Dispose() }
    if ($handler) { $handler.Dispose() }
    foreach ($owned in $script:ownedProcesses) { $owned.Process.Dispose() }
    Write-Host "Checks completed: $script:checks"
    foreach ($cleanupError in $cleanupErrors) { Write-Host "CLEANUP ERROR: $cleanupError" }
    Write-Host "Installer test logs retained at: $logDirectory"
    if ($transcriptStarted) { Stop-Transcript | Out-Null }
}
if ($failure) { throw $failure }
if ($cleanupErrors.Count -gt 0) { throw 'Installation checks passed but cleanup failed; see the retained installation log.' }
