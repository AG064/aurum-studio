# Install the verified Aurum application and its local rendering runtime.
# Existing projects, sessions, and client configuration are never migrated or deleted.
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$Binary,
    [string]$GodotBinary,
    [string]$StudioHome = 'A:\AurumStudio',
    [switch]$NoBuild,
    [switch]$NoEnvironment,
    [switch]$NoShortcut
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$StudioHome = [IO.Path]::GetFullPath($StudioHome)
if ($StudioHome.TrimEnd('\') -eq [IO.Path]::GetPathRoot($StudioHome).TrimEnd('\')) {
    throw 'StudioHome must be an application directory, not a drive root'
}
if ($WhatIfPreference) {
    Write-Host "Would install Aurum and its runtime under $StudioHome. Existing state would be preserved."
    return
}
if (-not $Binary) {
    $Binary = Join-Path $repo 'target/release/aurum.exe'
    if (-not $NoBuild) {
        & cargo build --locked --release -p aurum-cli --manifest-path (Join-Path $repo 'Cargo.toml')
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
}
$Binary = (Resolve-Path -LiteralPath $Binary).Path
if (-not $GodotBinary) {
    $GodotBinary = Join-Path (Split-Path $repo -Parent) 'godot/Godot_v4.7-stable_win64.exe'
}
$GodotBinary = (Resolve-Path -LiteralPath $GodotBinary).Path
if ((Get-Item -LiteralPath $GodotBinary).Length -lt 1MB) {
    throw 'Supply the full Godot runtime executable, not the small console launcher'
}
$sourceVersion=(& $Binary --version | Out-String).Trim()
if($sourceVersion -notmatch '^aurum (\d+\.\d+\.\d+)' -or [version]$Matches[1] -lt [version]'0.2.0'){
    throw 'This installer requires an Aurum Studio 0.2 or newer binary'
}
$licenseWork=Join-Path ([IO.Path]::GetTempPath()) ('aurum-runtime-notices-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $licenseWork,(Join-Path $licenseWork 'userdata')|Out-Null
$previousAppData=$env:APPDATA
try {
    $env:APPDATA=Join-Path $licenseWork 'userdata'
    & $Binary new (Join-Path $licenseWork 'project') --template 3d --name runtime-notices --json | Out-Null
    if($LASTEXITCODE){throw 'Could not prepare the runtime probe'}
    $noticeText=(& $Binary project (Join-Path $licenseWork 'project') --godot $GodotBinary --request-json '{"op":"runtime_info"}' | Out-String)
    if($LASTEXITCODE){throw 'The supplied runtime failed its compatibility probe'}
    $notices=$noticeText|ConvertFrom-Json -Depth 100
    if($notices.version.major -ne 4 -or $notices.version.minor -ne 7){throw 'This build is verified against Godot 4.7'}
} finally {$env:APPDATA=$previousAppData}
$binDirectory = Join-Path $StudioHome 'bin'
$runtimeDirectory = Join-Path $StudioHome 'runtime'
$backupDirectory = Join-Path $StudioHome 'backups'
if (-not $PSCmdlet.ShouldProcess($StudioHome,'Install Aurum and its rendering runtime')) { return }
New-Item -ItemType Directory -Path $binDirectory,$runtimeDirectory,$backupDirectory -Force | Out-Null

function Install-VerifiedFile([string]$Source,[string]$Destination) {
    $sourceHash = (Get-FileHash -LiteralPath $Source -Algorithm SHA256).Hash
    if ((Test-Path -LiteralPath $Destination -PathType Leaf) -and
        (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash -eq $sourceHash) { return $sourceHash }
    $stage = "$Destination.stage.$([guid]::NewGuid().ToString('N'))"
    Copy-Item -LiteralPath $Source -Destination $stage
    if ((Get-FileHash -LiteralPath $stage -Algorithm SHA256).Hash -ne $sourceHash) {
        throw "Staging hash mismatch; the existing file was not changed: $Destination"
    }
    if (Test-Path -LiteralPath $Destination) {
        $backup = Join-Path $backupDirectory ((Split-Path $Destination -Leaf)+'.'+[guid]::NewGuid().ToString('N'))
        [IO.File]::Replace($stage,$Destination,$backup,$true)
    } else {
        [IO.File]::Move($stage,$Destination)
    }
    return $sourceHash
}

$target = Join-Path $binDirectory 'aurum.exe'
$runtimeTarget = Join-Path $runtimeDirectory (Split-Path $GodotBinary -Leaf)
$runtimeHash = Install-VerifiedFile $GodotBinary $runtimeTarget
$binaryHash = Install-VerifiedFile $Binary $target
[IO.File]::WriteAllText((Join-Path $runtimeDirectory 'LICENSE-Godot.txt'),[string]$notices.license,[Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $runtimeDirectory 'third-party-licenses.json'),$noticeText,[Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination (Join-Path $StudioHome 'LICENSE-Aurum.txt') -Force
$version = (& $target --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw 'The installed executable failed its version probe' }

$launcher = Join-Path $StudioHome 'Launch Aurum Studio.vbs'
$quotedTarget = $target.Replace('"','""')
$quotedDirectory = $StudioHome.Replace('"','""')
$launchText = @"
Set shell = CreateObject("WScript.Shell")
Set environment = shell.Environment("Process")
environment("AURUM_STUDIO_HOME") = "$quotedDirectory"
shell.CurrentDirectory = "$quotedDirectory"
shell.Run Chr(34) & "$quotedTarget" & Chr(34) & " studio", 0, False
"@
[IO.File]::WriteAllText($launcher,$launchText,[Text.UTF8Encoding]::new($false))

$receipt = [ordered]@{
    version = $version
    installed_utc = [DateTime]::UtcNow.ToString('O')
    binary = $target
    binary_sha256 = $binaryHash
    runtime = $runtimeTarget
    runtime_sha256 = $runtimeHash
    launcher = $launcher
    state_preserved = $true
}
[IO.File]::WriteAllText((Join-Path $StudioHome 'install.json'),($receipt|ConvertTo-Json),[Text.UTF8Encoding]::new($false))

if (-not $NoEnvironment) {
    [Environment]::SetEnvironmentVariable('AURUM_STUDIO_HOME',$StudioHome,'User')
    $entries = @(([Environment]::GetEnvironmentVariable('Path','User') -split ';') | Where-Object { $_ })
    if (-not @($entries | Where-Object { $_.TrimEnd('\') -ieq $binDirectory.TrimEnd('\') }).Count) {
        [Environment]::SetEnvironmentVariable('Path',((@($entries)+$binDirectory)-join ';'),'User')
    }
}
if (-not $NoShortcut) {
    $programs = [Environment]::GetFolderPath('Programs')
    if ($programs) {
        $shortcutPath = Join-Path $programs 'Aurum Studio.lnk'
        if (Test-Path -LiteralPath $shortcutPath) {
            Copy-Item -LiteralPath $shortcutPath -Destination (Join-Path $backupDirectory ('Aurum Studio.'+[guid]::NewGuid().ToString('N')+'.lnk'))
        }
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut($shortcutPath)
        $shortcut.TargetPath = Join-Path $env:WINDIR 'System32/wscript.exe'
        $shortcut.Arguments = '"' + $launcher + '"'
        $shortcut.WorkingDirectory = $StudioHome
        $shortcut.Description = 'Aurum Studio'
        $shortcut.IconLocation = $target + ',0'
        $shortcut.Save()
    }
}
Write-Host "Installed $version"
Write-Host "Launch: $launcher"
Write-Host "Runtime included: $runtimeTarget"
Write-Host "Previous replaced files are retained under $backupDirectory"
