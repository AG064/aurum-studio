[CmdletBinding()]
param(
    [string]$AurumBinary = 'aurum',
    [string]$GodotBinary = $env:AURUM_GODOT,
    [switch]$Package,
    [string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'This verifier currently targets Windows and PowerShell 7.' }
if ($OutputDirectory -and -not $Package) { throw '-OutputDirectory requires -Package.' }
$AurumBinary = (Get-Command $AurumBinary -CommandType Application -ErrorAction Stop).Source
if (-not $GodotBinary) {
    $runtimeRoots = @((Join-Path (Split-Path (Split-Path $AurumBinary -Parent) -Parent) 'runtime'))
    if ($env:AURUM_STUDIO_HOME) { $runtimeRoots = @((Join-Path $env:AURUM_STUDIO_HOME 'runtime')) + $runtimeRoots }
    foreach ($runtimeRoot in $runtimeRoots) {
        $found = @(Get-ChildItem -LiteralPath $runtimeRoot -Filter 'Godot*.exe' -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Length -gt 1MB -and $_.Name -notmatch 'console' } | Sort-Object Name)
        if ($found.Count -gt 0) { $GodotBinary = $found[0].FullName; break }
    }
}
if (-not $GodotBinary) { throw 'Supply -GodotBinary with the full Godot 4.7 executable, or install Aurum with its runtime first.' }
$GodotBinary = (Resolve-Path -LiteralPath $GodotBinary).Path
$output = if ($OutputDirectory) { [IO.Path]::GetFullPath($OutputDirectory) } else { $null }
if ($output -and (Test-Path -LiteralPath $output)) { throw 'Output directory already exists. Choose a new destination.' }
$source = Split-Path $PSScriptRoot -Parent
$work = Join-Path ([IO.Path]::GetTempPath()) ('orbit-verification-' + [guid]::NewGuid().ToString('N'))
$project = Join-Path $work 'project'
New-Item -ItemType Directory -Path $project,(Join-Path $work 'userdata'),(Join-Path $work 'state') | Out-Null
$null = & robocopy $source $project /E /XD .godot .aurum dist .git /NFL /NDL /NJH /NJS
if ($LASTEXITCODE -ge 8) { throw 'Could not create the isolated project copy.' }
$previous = @{APPDATA=$env:APPDATA; AURUM_STUDIO_HOME=$env:AURUM_STUDIO_HOME; AURUM_GODOT=$env:AURUM_GODOT}
$env:APPDATA = Join-Path $work 'userdata'
$env:AURUM_STUDIO_HOME = Join-Path $work 'state'
$env:AURUM_GODOT = $GodotBinary
$results = [ordered]@{work=$work}
function Invoke-ProjectOperation($request) {
    $text = (& $AurumBinary project $project --request-json ($request | ConvertTo-Json -Depth 30 -Compress)) -join "`n"
    $code = $LASTEXITCODE
    [IO.File]::AppendAllText((Join-Path $work 'commands.log'),"$($request.op) exit=$code`n$text`n")
    if ($code -ne 0) { throw $text }
    return $text | ConvertFrom-Json -Depth 60
}
try {
    $results.validation = Invoke-ProjectOperation @{op='validate'}
    $results.menu = Invoke-ProjectOperation @{op='play';frames=5;fixed_fps=60}
    $results.gameplay = Invoke-ProjectOperation @{op='play';scene='tests/acceptance.tscn';frames=120;fixed_fps=60;user_args=@('--acceptance');report=$true}
    if (-not $results.gameplay.report.ok) { throw 'Gameplay report failed.' }
    $results.autoplay = Invoke-ProjectOperation @{op='play';frames=36000;fixed_fps=60;user_args=@('--autoplay');report=$true}
    $results.lance = Invoke-ProjectOperation @{op='play';frames=36000;fixed_fps=60;user_args=@('--autoplay','--weapon','1');report=$true}
    $results.arc = Invoke-ProjectOperation @{op='play';frames=36000;fixed_fps=60;user_args=@('--autoplay','--weapon','2');report=$true}
    foreach ($scenario in @('autoplay','lance','arc')) {
        $report = $results[$scenario].report
        if (-not $report.won -or $report.workshops -ne 4 -or $report.boss_phases -ne 3) { throw "$scenario did not complete all workshops and boss phases." }
    }
    $results.idle = Invoke-ProjectOperation @{op='play';frames=36000;fixed_fps=60;user_args=@('--idle-test');report=$true}
    if ($Package) { $results.package = Invoke-ProjectOperation @{op='package';output='dist/windows'} }
    if ($output) {
        if (Test-Path -LiteralPath $output) { throw 'The destination appeared during verification; refusing to overwrite it.' }
        New-Item -ItemType Directory -Path (Split-Path $output -Parent) -Force | Out-Null
        Copy-Item -LiteralPath $results.package.directory -Destination $output -Recurse
        foreach ($file in Get-ChildItem -LiteralPath $results.package.directory -File) {
            if ((Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath (Join-Path $output $file.Name)).Hash) {
                throw "Package copy failed verification: $($file.Name)"
            }
        }
        $results.output_directory = $output
    }
    $results.ok = $true
} catch {
    $results.ok = $false
    $results.error = $_.ToString()
} finally {
    [IO.File]::WriteAllText((Join-Path $work 'verification.json'),($results | ConvertTo-Json -Depth 80))
    $env:APPDATA = $previous.APPDATA
    $env:AURUM_STUDIO_HOME = $previous.AURUM_STUDIO_HOME
    $env:AURUM_GODOT = $previous.AURUM_GODOT
}
[pscustomobject]@{
    ok=$results.ok; work=$work; error=$results['error']; output_directory=$results['output_directory']
    gameplay_checks=if($results.Contains('gameplay')){$results.gameplay.report.check_count}else{0}
} | ConvertTo-Json
if (-not $results.ok) { exit 1 }
