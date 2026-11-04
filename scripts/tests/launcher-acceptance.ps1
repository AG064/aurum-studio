[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Binary,
    [Parameter(Mandatory)][string]$LauncherBinary,
    [Parameter(Mandatory)][string]$GodotBinary,
    [Parameter(Mandatory)][string]$TemplatesDirectory,
    [string]$SourceDirectory = (Join-Path $PSScriptRoot '../..'),
    [string]$WorkDirectory = ('A:/CodexData/temp/aurum-041-continuation-01a13506/launcher-acceptance-' + [guid]::NewGuid().ToString('N'))
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$work = [IO.Path]::GetFullPath($WorkDirectory)
$sourceRoot = (Resolve-Path -LiteralPath $SourceDirectory).Path
if (-not $sourceRoot.StartsWith('A:\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Launcher qualification source must stay on A:' }
if (-not $work.StartsWith('A:\CodexData\temp\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Launcher qualification must use a dedicated A: ephemeral directory' }
if (Test-Path -LiteralPath $work) { throw 'Fresh evidence directory required' }
New-Item -ItemType Directory -Path $work,(Join-Path $work 'tmp'),(Join-Path $work 'unrelated cwd') | Out-Null
$env:TEMP = Join-Path $work 'tmp'
$env:TMP = $env:TEMP
$checks = [Collections.Generic.List[object]]::new()
$owned = [Collections.Generic.List[object]]::new()
$cleanup = [Collections.Generic.List[object]]::new()
$completed = $false
$failureType = $null
function Track([Diagnostics.Process]$Process,[string]$Executable) {
    $owned.Add(@{process=$Process;home=(Split-Path $Executable -Parent);started=$Process.StartTime.ToUniversalTime();entry=$Executable;cli=(Join-Path (Split-Path $Executable -Parent) 'bin/aurum.exe')})
}
function Require([bool]$Condition,[string]$Name) {
    $checks.Add(@{name=$Name;passed=$Condition})
    if (-not $Condition) { throw $Name }
}
function Probe([string]$Executable,[string]$Argument) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = Join-Path $work 'unrelated cwd'
    $start.ArgumentList.Add($Argument)
    $process = [Diagnostics.Process]::Start($start)
    Track $process $Executable
    $output = $process.StandardOutput.ReadToEndAsync()
    $errors = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(20000)) { $process.Kill(); throw 'Owned nonlaunch probe exceeded deadline' }
    $text = $output.GetAwaiter().GetResult()
    $errorText = $errors.GetAwaiter().GetResult()
    Require ($text.Length -le 4096 -and $errorText.Length -le 4096) 'Nonlaunch output is bounded'
    return @{exit=$process.ExitCode;text=$text.Trim();error_bytes=$errorText.Length}
}
function Gui([string]$Executable) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.WorkingDirectory = Join-Path $work 'unrelated cwd'
    $process = [Diagnostics.Process]::Start($start)
    Track $process $Executable
    return $process
}
function ActualHome([string]$studioDirectory,[string]$Name) {
    $process = Gui (Join-Path $studioDirectory 'Aurum Studio.exe')
    $ready = Receipt $studioDirectory $process.Id
    Require ($ready.ok -and -not $ready.reused -and $ready.listener_pid -eq $ready.child_pid) ($Name + ': exact owned readiness')
    $project = Join-Path $studioDirectory 'projects/welcome'
    $endpoint = Get-Content -LiteralPath (Join-Path $project '.aurum/native/host.json') -Raw | ConvertFrom-Json
    $secret = (Get-Content -LiteralPath (Join-Path $project '.aurum/native/host.token') -Raw).Trim()
    Require ($endpoint.owner.pid -eq $ready.child_pid) ($Name + ': metadata ownership matches')
    $base = 'http://127.0.0.1:' + $endpoint.port
    $state = Invoke-RestMethod ($base + '/api/state') -Headers @{'X-Aurum-Token'=$secret} -TimeoutSec 5
    Require ($state.studio -eq '0.4.1' -and [IO.Path]::GetFullPath($state.root) -eq [IO.Path]::GetFullPath($project)) ($Name + ': actual portable backend and version')
    Require (-not $ready.raw_output_retained -and -not $ready.credentials_retained) ($Name + ': startup privacy')
    $null = Invoke-RestMethod ($base + '/api/stop') -Method Post -Headers @{'X-Aurum-Token'=$secret} -TimeoutSec 5
    Require ($process.WaitForExit(20000) -and $process.ExitCode -eq 0) ($Name + ': authenticated shutdown')
}
function RetireOwned {
    foreach ($record in $owned) {
        $parent = $record.process
        $queryVerified = $true
        $backendVerdicts = [Collections.Generic.List[object]]::new()
        for ($pass=0; $pass -lt 2; $pass++) {
            try {
                $children = @(Get-CimInstance -ClassName Win32_Process -Filter ('ParentProcessId=' + $parent.Id) -OperationTimeoutSec 2 -ErrorAction Stop)
                foreach ($item in $children) {
                    $created = $item.CreationDate.ToUniversalTime()
                    if ($created -lt $record.started -or $created -gt $record.started.AddSeconds(20)) { continue }
                    $parent.Refresh()
                    if ($parent.HasExited -and $created -gt $parent.ExitTime.ToUniversalTime()) { continue }
                    if ($item.Name -notin @('aurum.exe','Aurum Studio.exe','aurum-studio.exe')) { continue }
                    if (-not $item.ExecutablePath) { $queryVerified=$false; continue }
                    $image=[IO.Path]::GetFullPath($item.ExecutablePath)
                    $isCli=$image -eq [IO.Path]::GetFullPath($record.cli)
                    if (-not $isCli -and $image -ne [IO.Path]::GetFullPath($record.entry)) { continue }
                    $backend = Get-Process -Id $item.ProcessId -ErrorAction SilentlyContinue
                    if (-not $backend) { continue }
                    if ([Math]::Abs($backend.StartTime.ToUniversalTime().Ticks - $created.Ticks) -ge 10) { $queryVerified=$false; continue }
                    $method = 'authenticated_stop'
                    $project = Join-Path $record.home 'projects/welcome'
                    try {
                        if (-not $isCli) { throw 'owned_browser_helper' }
                        $metaPath = Join-Path $project '.aurum/native/host.json'
                        $tokenPath = Join-Path $project '.aurum/native/host.token'
                        if ((Get-Item -LiteralPath $metaPath).Length -gt 8192 -or (Get-Item -LiteralPath $tokenPath).Length -gt 4096) { throw 'metadata_bound' }
                        $meta = Get-Content -LiteralPath $metaPath -Raw | ConvertFrom-Json
                        $secret = (Get-Content -LiteralPath $tokenPath -Raw).Trim()
                        if ($meta.owner.pid -ne $backend.Id -or [IO.Path]::GetFullPath($meta.owner.project) -ne [IO.Path]::GetFullPath($project) -or [IO.Path]::GetFullPath($meta.owner.executable) -ne [IO.Path]::GetFullPath($record.cli) -or $meta.port -lt 1 -or $meta.port -gt 65535 -or $secret -notmatch '^[0-9a-f]{64}$') { throw 'metadata_identity' }
                        $null = Invoke-RestMethod ('http://127.0.0.1:' + $meta.port + '/api/stop') -Method Post -Headers @{'X-Aurum-Token'=$secret} -TimeoutSec 2
                    } catch { $method = 'retained_owned_handle' }
                    if (-not $backend.WaitForExit(3000)) { $method='retained_owned_handle'; $backend.Kill() }
                    $gone = $backend.WaitForExit(2000)
                    $backendVerdicts.Add(@{pid=$backend.Id;identity_verified=$true;exit_verified=$gone;method=$method})
                }
            } catch { $queryVerified=$false }
            if ($pass -eq 0) {
                if (-not $parent.WaitForExit(2000)) {
                    $script:dialogPid=$parent.Id
                    $close = [AurumLauncherWindows+EnumCallback]{param($window,$data)
                        [uint32]$owner=0
                        $null=[AurumLauncherWindows]::GetWindowThreadProcessId($window,[ref]$owner)
                        if ($owner -eq $script:dialogPid) { $null=[AurumLauncherWindows]::PostMessage($window,0x10,[IntPtr]::Zero,[IntPtr]::Zero) }
                        return $true
                    }
                    $null=[AurumLauncherWindows]::EnumWindows($close,[IntPtr]::Zero)
                    if (-not $parent.WaitForExit(2000)) { $parent.Kill() }
                }
            }
        }
        $launcherGone=$parent.WaitForExit(2000)
        $cleanup.Add(@{launcher_pid=$parent.Id;launcher_exit_verified=$launcherGone;child_query_verified=$queryVerified;backends=$backendVerdicts;owned_cleanup_verified=($launcherGone -and $queryVerified -and @($backendVerdicts|Where-Object{-not $_.exit_verified}).Count -eq 0)})
    }
}
function Receipt([string]$studioDirectory,[int]$LauncherPid) {
    $path = Join-Path $studioDirectory 'launcher-status.json'
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Test-Path -LiteralPath $path) {
            Require ((Get-Item -LiteralPath $path).Length -le 4096) 'Startup evidence is bounded'
            $value = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
            if ($value.launcher_pid -eq $LauncherPid) { return $value }
        }
        Start-Sleep -Milliseconds 100
    }
    throw 'Owned launcher readiness receipt unavailable'
}
function Subsystem([string]$Executable) {
    $file = [IO.File]::OpenRead($Executable)
    try {
        $reader = [IO.BinaryReader]::new($file)
        $file.Position = 0x3c
        $offset = $reader.ReadInt32()
        Require ($offset -ge 64 -and $offset -lt 65536 -and $offset + 96 -lt $file.Length) 'PE header location is bounded'
        $file.Position = $offset
        Require ($reader.ReadUInt32() -eq 0x4550) 'Windows PE signature exists'
        $file.Position = $offset + 4 + 20 + 68
        return $reader.ReadUInt16()
    } finally { $file.Dispose() }
}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class AurumLauncherWindows {
  public delegate bool EnumCallback(IntPtr window, IntPtr data);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumCallback callback, IntPtr data);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, StringBuilder text, int size);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
}
'@
$userPathBefore = [Environment]::GetEnvironmentVariable('Path','User')
$userHomeBefore = [Environment]::GetEnvironmentVariable('AURUM_STUDIO_HOME','User')
$studioDirectory = Join-Path $work 'Studio with spaces ü'
$script:activeHost = $null
$script:activeToken = $null
try {
    $shortcutDirectory=Join-Path $work 'isolated start menu'
    & (Join-Path $sourceRoot 'scripts/install.ps1') -Binary $Binary -LauncherBinary $LauncherBinary -GodotBinary $GodotBinary -StudioHome $studioDirectory -NoEnvironment -ShortcutDirectory $shortcutDirectory *> (Join-Path $work 'install.log')
    Require ($LASTEXITCODE -eq 0) 'Fresh A-only installation succeeds'
    $entry = Join-Path $studioDirectory 'Aurum Studio.exe'
    $shortcut=(New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $shortcutDirectory 'Aurum Studio.lnk'))
    Require ($shortcut.TargetPath -eq $entry -and -not $shortcut.Arguments -and $shortcut.WorkingDirectory -eq $studioDirectory) 'Isolated shortcut targets root GUI executable directly'
    Require ((Subsystem $entry) -eq 2) 'Studio entrypoint uses Windows GUI subsystem'
    Require ((Subsystem (Join-Path $studioDirectory 'bin/aurum.exe')) -eq 3) 'CLI retains console subsystem'
    $version = Probe $entry '--version'
    Require ($version.exit -eq 0 -and $version.text -eq 'aurum-studio 0.4.1') 'Launcher reports0.4.1'
    $check = Probe $entry '--check'
    $checkValue = $check.text | ConvertFrom-Json
    Require ($check.exit -eq 0 -and $checkValue.ok -and -not $checkValue.launch_performed) 'Package check performs no Studio launch'
    Require (-not (Test-Path -LiteralPath (Join-Path $studioDirectory 'projects/welcome'))) 'Package check creates no welcome project'
    $invalid = Probe $entry '--url=http://untrusted.invalid'
    Require ($invalid.exit -eq 64) 'Arbitrary URL and CLI flags refused'
    $install = Get-Content -LiteralPath (Join-Path $studioDirectory 'install.json') -Raw | ConvertFrom-Json
    Require ($install.launcher_sha256 -eq (Get-FileHash -LiteralPath $entry).Hash) 'Installed launcher hash matches receipt'
    Require ($install.binary_sha256 -eq (Get-FileHash -LiteralPath (Join-Path $studioDirectory 'bin/aurum.exe')).Hash) 'Installed CLI hash matches receipt'

    $launch = Gui $entry
    $ready = Receipt $studioDirectory $launch.Id
    Require ($ready.ok -and $ready.stage -eq 'browser_requested' -and $ready.listener_pid -eq $ready.child_pid -and -not $ready.reused) 'Owned child listener and browser request are verified'
    Require (-not $ready.raw_output_retained -and -not $ready.credentials_retained) 'Startup report retains no raw output or credentials'
    $project = Join-Path $studioDirectory 'projects/welcome'
    $script:activeHost = Get-Content -LiteralPath (Join-Path $project '.aurum/native/host.json') -Raw | ConvertFrom-Json
    $script:activeToken = (Get-Content -LiteralPath (Join-Path $project '.aurum/native/host.token') -Raw).Trim()
    Require ($script:activeHost.owner.pid -eq $ready.child_pid) 'Readiness matches exact host metadata PID'
    $base = 'http://127.0.0.1:' + $script:activeHost.port
    $state = Invoke-RestMethod ($base + '/api/state') -Headers @{'X-Aurum-Token'=$script:activeToken} -TimeoutSec 5
    Require ($state.studio -eq '0.4.1' -and [IO.Path]::GetFullPath($state.root) -eq [IO.Path]::GetFullPath($project)) 'Actual backend serves0.4.1 and portable welcome project'
    $ownedStart = (Get-Process -Id $ready.child_pid).StartTime.ToUniversalTime().Ticks
    $duplicate = Gui $entry
    $reuse = Receipt $studioDirectory $duplicate.Id
    Require ($duplicate.WaitForExit(20000) -and $duplicate.ExitCode -eq 0 -and $reuse.reused -and $reuse.listener_pid -eq $ready.child_pid) 'Duplicate launch reuses accepted host and exits'
    Require ((Get-Process -Id $ready.child_pid).StartTime.ToUniversalTime().Ticks -eq $ownedStart) 'Duplicate launch preserves original process identity'
    $null = Invoke-RestMethod ($base + '/api/stop') -Method Post -Headers @{'X-Aurum-Token'=$script:activeToken} -TimeoutSec 5
    Require ($launch.WaitForExit(20000) -and $launch.ExitCode -eq 0) 'Authenticated shutdown retires GUI and owned backend'
    $script:activeHost = $null
    $script:activeToken = $null

    $relocated = Join-Path $work 'Relocated Studio ü'
    New-Item -ItemType Directory -Path $relocated | Out-Null
    Copy-Item -LiteralPath $entry -Destination (Join-Path $relocated 'Aurum Studio.exe')
    Copy-Item -LiteralPath (Join-Path $studioDirectory 'bin'),(Join-Path $studioDirectory 'runtime') -Destination $relocated -Recurse
    $relocation = Probe (Join-Path $relocated 'Aurum Studio.exe') '--check'
    Require ($relocation.exit -eq 0 -and ($relocation.text | ConvertFrom-Json).ok) 'Program files remain relocatable from unrelated cwd'
    ActualHome $relocated 'Relocated package'

    $release=Join-Path $work 'release output'
    & (Join-Path $sourceRoot 'scripts/package-release.ps1') -Binary $Binary -LauncherBinary $LauncherBinary -GodotBinary $GodotBinary -TemplatesDirectory $TemplatesDirectory -OutputDirectory $release *> (Join-Path $work 'package.log')
    Require ($LASTEXITCODE -eq 0) 'Actual release packaging succeeds'
    $archive=Join-Path $release 'Aurum-Studio-0.4.1-windows-x64.zip'
    $extracted=Join-Path $work 'Fresh ZIP Studio ü'
    [IO.Compression.ZipFile]::ExtractToDirectory($archive,$extracted)
    Require ((Get-FileHash -LiteralPath (Join-Path $extracted 'Aurum Studio.exe')).Hash -eq (Get-FileHash -LiteralPath $entry).Hash) 'Fresh ZIP retains exact GUI executable'
    ActualHome $extracted 'Fresh ZIP extraction'

    $incomplete = Join-Path $work 'Incomplete Studio'
    New-Item -ItemType Directory -Path $incomplete | Out-Null
    $badEntry = Join-Path $incomplete 'Aurum Studio.exe'
    Copy-Item -LiteralPath $entry -Destination $badEntry
    $missing = Probe $badEntry '--check'
    Require ($missing.exit -ne 0 -and ($missing.text | ConvertFrom-Json).reason -eq 'missing_or_unsafe_cli_layout') 'Missing CLI reports a concrete nonlaunch failure'
    $badGui = Gui $badEntry
    $script:dialog = [IntPtr]::Zero
    $script:dialogPid = $badGui.Id
    $callback = [AurumLauncherWindows+EnumCallback]{param($window,$data)
        [uint32]$owner = 0
        $null = [AurumLauncherWindows]::GetWindowThreadProcessId($window,[ref]$owner)
        if ($owner -eq $script:dialogPid -and [AurumLauncherWindows]::IsWindowVisible($window)) {
            $title = [Text.StringBuilder]::new(256)
            $null = [AurumLauncherWindows]::GetWindowText($window,$title,256)
            if ($title.ToString() -eq 'Aurum Studio') { $script:dialog = $window; return $false }
        }
        return $true
    }
    $dialogDeadline = [DateTime]::UtcNow.AddSeconds(10)
    while ($script:dialog -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $dialogDeadline) {
        $null = [AurumLauncherWindows]::EnumWindows($callback,[IntPtr]::Zero)
        Start-Sleep -Milliseconds 100
    }
    Require ($script:dialog -ne [IntPtr]::Zero) 'Actual missing-CLI launch shows visible owned error dialog'
    $null = [AurumLauncherWindows]::PostMessage($script:dialog,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
    Require ($badGui.WaitForExit(10000) -and $badGui.ExitCode -ne 0) 'Owned failure dialog exits after dismissal'
    Require ([Environment]::GetEnvironmentVariable('Path','User') -eq $userPathBefore -and [Environment]::GetEnvironmentVariable('AURUM_STUDIO_HOME','User') -eq $userHomeBefore) 'User environment remains unchanged'
    $completed=$true
} catch { $failureType=$_.Exception.GetType().Name; throw } finally {
    RetireOwned
    $retired=@($cleanup|Where-Object{-not $_.owned_cleanup_verified}).Count -eq 0
    @{ok=($completed -and $retired);checks=$checks;cleanup=$cleanup;failure_type=$failureType;directory=$work;binary_sha256=(Get-FileHash -LiteralPath $Binary).Hash;launcher_sha256=(Get-FileHash -LiteralPath $LauncherBinary).Hash;browser_requested=$completed;browser_rendering_verified=$false;signing_verified=$false}|ConvertTo-Json -Depth 10|Set-Content -LiteralPath (Join-Path $work 'verification.json') -Encoding utf8
    if (-not $retired) { throw 'Owned launcher/child cleanup could not be verified; failure receipt retained' }
}
Write-Output ('AURUM_LAUNCHER_ACCEPTANCE_OK evidence=' + (Join-Path $work 'verification.json'))
