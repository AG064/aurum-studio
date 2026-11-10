# Install the verified Aurum application and its local rendering runtime.
# Existing projects, sessions, and client configuration are never migrated or deleted.
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$Binary,
    [string]$LauncherBinary,
    [string]$GodotBinary,
    [string]$StudioHome = 'A:\AurumStudio',
    [switch]$NoBuild,
    [switch]$NoEnvironment,
    [string]$ShortcutDirectory,
    [switch]$NoShortcut
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
. (Join-Path $PSScriptRoot 'release/binary-version.ps1')
$StudioHome = [IO.Path]::GetFullPath($StudioHome)
if ($StudioHome.TrimEnd('\') -eq [IO.Path]::GetPathRoot($StudioHome).TrimEnd('\')) {
    throw 'StudioHome must be an application directory, not a drive root'
}
if ($WhatIfPreference) {
    Write-Host "Would install Aurum and its runtime under $StudioHome. Existing state would be preserved."
    return
}
if (-not $Binary) {
    $buildTarget = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR, $repo) } else { Join-Path $repo 'target' }
    $Binary = Join-Path $buildTarget 'release/aurum.exe'
    if (-not $NoBuild) {
        & cargo build --locked --release -p aurum-cli --bins --manifest-path (Join-Path $repo 'Cargo.toml')
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
}
$Binary = (Resolve-Path -LiteralPath $Binary).Path
if (-not $LauncherBinary) { $LauncherBinary = Join-Path (Split-Path $Binary -Parent) 'aurum-studio.exe' }
$LauncherBinary = (Resolve-Path -LiteralPath $LauncherBinary).Path
if (-not $GodotBinary) {
    $GodotBinary = Join-Path (Split-Path $repo -Parent) 'godot/Godot_v4.7-stable_win64.exe'
}
$GodotBinary = (Resolve-Path -LiteralPath $GodotBinary).Path
if ((Get-Item -LiteralPath $GodotBinary).Length -lt 1MB) {
    throw 'Supply the full Godot runtime executable, not the small console launcher'
}
$cliVersion=Get-AurumBinaryVersion -Path $Binary -Prefix 'aurum'
if([version]$cliVersion -lt [version]'0.4.1'){
    throw 'This installer requires an Aurum Studio 0.4.1 or newer binary'
}
$guiVersion=Get-AurumBinaryVersion -Path $LauncherBinary -Prefix 'aurum-studio'
if($guiVersion -ne $cliVersion){
    throw 'The Studio launcher and CLI must have exactly matching versions'
}
$launcherVersion='aurum-studio ' + $guiVersion
$licenseWork=Join-Path ([IO.Path]::GetTempPath()) ('aurum-runtime-notices-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $licenseWork,(Join-Path $licenseWork 'userdata')|Out-Null
$previousAppData=$env:APPDATA
$previousStudioHome=$env:AURUM_STUDIO_HOME
try {
    $env:APPDATA=Join-Path $licenseWork 'userdata'
    $env:AURUM_STUDIO_HOME=$licenseWork
    & $Binary new (Join-Path $licenseWork 'project') --template 3d --name runtime-notices --json | Out-Null
    if($LASTEXITCODE){throw 'Could not prepare the runtime probe'}
    $noticeText=(& $Binary project (Join-Path $licenseWork 'project') --godot $GodotBinary --request-json '{"op":"runtime_info"}' | Out-String)
    if($LASTEXITCODE){throw 'The supplied runtime failed its compatibility probe'}
    $notices=$noticeText|ConvertFrom-Json -Depth 100
    if($notices.version.major -ne 4 -or $notices.version.minor -ne 7){throw 'This build is verified against Godot 4.7'}
} finally {$env:APPDATA=$previousAppData; $env:AURUM_STUDIO_HOME=$previousStudioHome}
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
$runtimeTarget = Join-Path $runtimeDirectory 'godot.exe'
$runtimeHash = Install-VerifiedFile $GodotBinary $runtimeTarget
$binaryHash = Install-VerifiedFile $Binary $target
$guiLauncher = Join-Path $StudioHome 'Aurum Studio.exe'
$launcherHash = Install-VerifiedFile $LauncherBinary $guiLauncher
[IO.File]::WriteAllText((Join-Path $runtimeDirectory 'LICENSE-Godot.txt'),[string]$notices.license,[Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $runtimeDirectory 'third-party-licenses.json'),$noticeText,[Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination (Join-Path $StudioHome 'LICENSE-Aurum.txt') -Force
$installedVersion=Get-AurumBinaryVersion -Path $target -Prefix 'aurum'
if ($installedVersion -ne $cliVersion) { throw 'The installed executable failed its version identity probe' }
$version='aurum ' + $installedVersion

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
    launcher = $guiLauncher
    launcher_sha256 = $launcherHash
    launcher_version = $launcherVersion
    compatibility_launcher = $launcher
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
    $programs = if ($ShortcutDirectory) { [IO.Path]::GetFullPath($ShortcutDirectory) } else { [Environment]::GetFolderPath('Programs') }
    if ($programs) {
        if ($programs.TrimEnd('\','/') -eq [IO.Path]::GetPathRoot($programs).TrimEnd('\','/')) { throw 'ShortcutDirectory must be a dedicated directory, not a drive root' }
        New-Item -ItemType Directory -Path $programs -Force | Out-Null
        $shortcutPath = Join-Path $programs 'Aurum Studio.lnk'
        if (Test-Path -LiteralPath $shortcutPath) {
            Copy-Item -LiteralPath $shortcutPath -Destination (Join-Path $backupDirectory ('Aurum Studio.'+[guid]::NewGuid().ToString('N')+'.lnk'))
        }
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut($shortcutPath)
        $shortcut.TargetPath = $guiLauncher
        $shortcut.Arguments = ''
        $shortcut.WorkingDirectory = $StudioHome
        $shortcut.Description = 'Aurum Studio'
        $shortcut.IconLocation = $guiLauncher + ',0'
        $shortcut.Save()
    }
}
Write-Host "Installed $version"
Write-Host "Launch: $guiLauncher"
Write-Host "Runtime included: $runtimeTarget"
Write-Host "Previous replaced files are retained under $backupDirectory"
