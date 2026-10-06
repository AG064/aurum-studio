[CmdletBinding()]
param(
    [string]$AurumBinary = 'aurum',
    [string]$GodotBinary = $env:AURUM_GODOT,
    [switch]$Package,
    [switch]$Render
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'This acceptance gate currently targets Windows.' }
$AurumBinary = (Get-Command $AurumBinary -CommandType Application -ErrorAction Stop).Source
if (-not $GodotBinary) { throw 'Supply the full Godot executable with -GodotBinary.' }
$GodotBinary = (Resolve-Path -LiteralPath $GodotBinary).Path
$taskWork = Join-Path ([IO.Path]::GetTempPath()) ('relay-yard-verification-' + [guid]::NewGuid().ToString('N'))
$taskProject = Join-Path $taskWork 'project'
New-Item -ItemType Directory -Path $taskProject,(Join-Path $taskWork 'state'),(Join-Path $taskWork 'userdata') | Out-Null
$null = & robocopy (Split-Path $PSScriptRoot -Parent) $taskProject /E /XD .godot .aurum dist .git /NFL /NDL /NJH /NJS
if ($LASTEXITCODE -ge 8) { throw 'Could not create the disposable project.' }
$previous = @{APPDATA=$env:APPDATA; LOCALAPPDATA=$env:LOCALAPPDATA; AURUM_GODOT=$env:AURUM_GODOT; AURUM_STUDIO_HOME=$env:AURUM_STUDIO_HOME}
$env:APPDATA = Join-Path $taskWork 'userdata'
$env:LOCALAPPDATA = $env:APPDATA
$env:AURUM_GODOT = $GodotBinary
$env:AURUM_STUDIO_HOME = Join-Path $taskWork 'state'
$results = [ordered]@{work=$taskWork; project=$taskProject; ok=$false}
function Invoke-Operation($request) {
    $text = (& $AurumBinary project $taskProject --request-json ($request | ConvertTo-Json -Depth 40 -Compress)) -join "`n"
    $code = $LASTEXITCODE
    [IO.File]::AppendAllText((Join-Path $taskWork 'commands.log'), "$($request.op) exit=$code`n$text`n")
    if ($code) { throw $text }
    return $text | ConvertFrom-Json -Depth 70
}
function Invoke-OwnedProcess([string]$Binary, [string[]]$Arguments, [int]$Seconds, [string]$Log) {
    $start = [Diagnostics.ProcessStartInfo]::new($Binary)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.RedirectStandardInput = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $started = $false
    try {
        if (-not $process.Start()) { throw 'Process did not start.' }
        $started = $true
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if ($Binary -eq $AurumBinary) {
            foreach ($message in @(
                @{jsonrpc='2.0';id=1;method='initialize';params=@{protocolVersion='2025-06-18'}},
                @{jsonrpc='2.0';id=2;method='tools/list'},
                @{jsonrpc='2.0';id=3;method='tools/call';params=@{name='aurum_project_query';arguments=@{op='describe';operation='capture'}}}
            )) { $process.StandardInput.WriteLine(($message | ConvertTo-Json -Depth 10 -Compress)) }
        }
        $process.StandardInput.Close()
        if (-not $process.WaitForExit($Seconds * 1000)) { $process.Kill($true); $process.WaitForExit(); throw 'Owned process exceeded its wall budget.' }
        $output = $stdout.GetAwaiter().GetResult()
        $errorText = $stderr.GetAwaiter().GetResult()
        [IO.File]::WriteAllText($Log, $output + "`n" + $errorText)
        if ($process.ExitCode -ne 0 -or $errorText.Contains('ERROR:')) { throw "Process failed. Evidence: $Log" }
        return $output
    } finally { if ($started -and -not $process.HasExited) { $process.Kill($true); $process.WaitForExit() }; $process.Dispose() }
}
try {
    $results.discovery = Invoke-Operation @{op='describe';operation='capture'}
    if ($results.discovery.input_schema.properties.frames.maximum -ne 3600000) { throw 'Frame budget discovery drifted.' }
    $mcpText = Invoke-OwnedProcess $AurumBinary @('mcp','--root',$taskProject,'--tools','studio') 20 (Join-Path $taskWork 'mcp.log')
    $mcp = @($mcpText.Trim() -split "`n" | ForEach-Object { $_ | ConvertFrom-Json -Depth 70 })
    $action = @($mcp[1].result.tools | Where-Object name -EQ 'aurum_project_action')[0]
    if ('capture' -notin $action.inputSchema.properties.op.enum -or 'web_build' -notin $action.inputSchema.properties.op.enum) { throw 'MCP operation discovery is incomplete.' }
    if ($mcp[2].result.structuredContent.op -ne 'capture') { throw 'MCP structured discovery failed.' }
    $results.mcp = @{ok=$true; structured=$true; capture=$true; web_build=$true}
    $results.validation = Invoke-Operation @{op='validate'}
    $created = Invoke-Operation @{op='scene_create';scene='authored.tscn';root_type='Node3D';name='AuthoredAsset'}
    $results.authored = Invoke-Operation @{op='scene_edit';scene='authored.tscn';expected_sha256=$created.sha256;operations=@(
        @{op='instance';parent='.';name='Generator';source='res://assets/models/generator.glb';properties=@{position=@{x=2;y=0;z=0};scale=@{x=1;y=1;z=1}}},
        @{op='create';parent='.';name='Camera';type='Camera3D';properties=@{position=@{x=0;y=2;z=5}}}
    )}
    $results.reopened = Invoke-Operation @{op='scene_inspect';scene='authored.tscn'}
    if ($results.reopened.tree.children.Count -ne 2) { throw 'Authored scene did not survive reopening.' }
    $file = Invoke-Operation @{op='read';path='godot/yard.gd'}
    $saved = Invoke-Operation @{op='write';path='godot/yard.gd';text=($file.text + "`n# Agent save/undo acceptance.`n");expected_sha256=$file.sha256}
    $restored = Invoke-Operation @{op='undo';path='godot/yard.gd';expected_sha256=$saved.sha256}
    if ($restored.sha256 -ne $file.sha256) { throw 'Agent file undo did not restore the original source.' }
    $results.checkpoints = Invoke-Operation @{op='play';scene='tests/acceptance.tscn';frames=300;fixed_fps=60;report=$true}
    $results.campaign = Invoke-Operation @{op='play';frames=18000;fixed_fps=60;timeout_seconds=180;user_args=@('--autoplay');report=$true}
    if (-not $results.campaign.report.won -or $results.campaign.report.powered -ne 3 -or $results.campaign.report.interactions -ne 6 -or $results.campaign.report.distance -lt 100 -or -not $results.campaign.report.boss_defeated -or $results.campaign.report.kills -lt 12 -or $results.campaign.report.extraction -lt 22 -or $results.campaign.report.extraction_waves -ne 3) { throw 'Physics-driven campaign did not complete all encounters and extraction waves.' }
    $results.idle = Invoke-Operation @{op='play';frames=4500;fixed_fps=60;user_args=@('--idle-test');report=$true}
    if ($results.idle.report.phase -ne 'lost' -or $results.idle.report.damage_taken -lt 100) { throw 'Idle courier did not face real danger.' }
    if ($Render) {
        $results.rendered = Invoke-Operation @{op='capture';frames=240;fixed_fps=60;capture_frames=@(1,90,240);events=@(
            @{frame=5;type='action';action='start';pressed=$true},
            @{frame=6;type='action';action='start';pressed=$false},
            @{frame=10;type='action';action='move_forward';pressed=$true},
            @{frame=50;type='action';action='move_forward';pressed=$false},
            @{frame=20;type='action';action='fire';pressed=$true},
            @{frame=80;type='action';action='fire';pressed=$false},
            @{frame=30;type='action';action='dash';pressed=$true},
            @{frame=31;type='action';action='dash';pressed=$false},
            @{frame=100;type='action';action='emp';pressed=$true},
            @{frame=101;type='action';action='emp';pressed=$false},
            @{frame=150;type='property';changes=@(@{path='.';property='move_speed';value=8.0})}
        )}
        $state = @{}; foreach ($property in $results.rendered.runtime.last_state.properties) { $state[$property.name] = $property.value }
        if ($state.phase -ne 'play' -or $state.observed_player.shots_fired -lt 1 -or $state.observed_player.dash_count -ne 1 -or $state.observed_player.emp_count -ne 1 -or $state.observed_player.travelled -lt 2 -or $state.move_speed -ne 8.0 -or $results.rendered.runtime.captures.Count -ne 3) { throw 'Rendered controls, combat and live tuning did not reach the game.' }
    }
    if ($Package) {
        $results.package = Invoke-Operation @{op='package';output='dist/windows'}
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../Play Relay Yard.vbs') -Destination $results.package.directory
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../godot/LICENSE-assets.txt') -Destination $results.package.directory
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../godot/assets/fonts/OFL.txt') -Destination $results.package.directory
        $packageReport = Join-Path $taskWork 'packaged-report.json'
        $null = Invoke-OwnedProcess $results.package.executable @('--headless','--fixed-fps','60','--quit-after','18010','--','--autoplay','--aurum-report',$packageReport) 90 (Join-Path $taskWork 'packaged-game.log')
        $results.packaged_game = Get-Content -LiteralPath $packageReport -Raw | ConvertFrom-Json -Depth 40
        if (-not $results.packaged_game.ok -or -not $results.packaged_game.won) { throw 'The packaged game did not complete.' }
    }
    $results.ok = $true
} catch {
    $results.error = $_.ToString()
} finally {
    [IO.File]::WriteAllText((Join-Path $taskWork 'verification.json'), ($results | ConvertTo-Json -Depth 90))
    foreach ($key in $previous.Keys) { [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process') }
}
[pscustomobject]@{ok=$results.ok;work=$taskWork;checks=if($results.Contains('checkpoints')){$results.checkpoints.report.check_count}else{0};error=$results['error']} | ConvertTo-Json
if (-not $results.ok) { exit 1 }
