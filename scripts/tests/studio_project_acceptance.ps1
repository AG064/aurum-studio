[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$AurumBinary,
    [Parameter(Mandatory)][string]$GodotBinary,
    [switch]$Package
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$AurumBinary = (Resolve-Path $AurumBinary).Path
$GodotBinary = (Resolve-Path $GodotBinary).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ('aurum-project-acceptance-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work,(Join-Path $work 'state'),(Join-Path $work 'userdata') | Out-Null
$env:AURUM_STUDIO_HOME = Join-Path $work 'state'
$env:AURUM_GODOT = $GodotBinary
$env:APPDATA = Join-Path $work 'userdata'
$project = Join-Path $work 'sample'
$checks = [Collections.Generic.List[string]]::new()
$server = $null

function Check([bool]$Condition,[string]$Message) {
    if (-not $Condition) { throw $Message }
    $checks.Add($Message)
}
function Invoke-AurumTest([string[]]$Arguments,[switch]$FailureExpected) {
    $lines = @(& $AurumBinary @Arguments 2>&1)
    $code = $LASTEXITCODE
    $text = ($lines | ForEach-Object { [string]$_ }) -join "`n"
    [IO.File]::AppendAllText((Join-Path $work 'commands.log'),"exit=$code $($Arguments[0])`n$text`n")
    if (-not $FailureExpected -and $code -ne 0) { throw $text }
    if ($FailureExpected -and $code -eq 0) { throw 'Expected a failing exit status' }
    return $text | ConvertFrom-Json -Depth 100
}
function Op($Request,[switch]$FailureExpected) {
    return Invoke-AurumTest @('project',$project,'--request-json',($Request | ConvertTo-Json -Compress -Depth 30)) -FailureExpected:$FailureExpected
}
try {
    $null = Invoke-AurumTest @('new',$project,'--template','3d','--name','sample','--json')
    $status=Op @{op='status'}
    Check $status.headless 'Project is available headlessly'
    $scene=Op @{op='scene_inspect';scene='main.tscn'}
    Check ($scene.tree.children.Count -eq 3) '3D starter has camera, light, and mesh'
    $changed=Op @{op='scene_edit';scene='main.tscn';expected_sha256=$scene.sha256;operations=@(@{op='create';parent='.';name='Probe';type='MeshInstance3D';properties=@{position=@{x=2;y=1;z=0};mesh=@{resource='BoxMesh'}}})}
    Check $changed.saved 'Scene batch saved successfully'
    $reopened=Op @{op='scene_inspect';scene='main.tscn'}
    $probe=@($reopened.tree.children | Where-Object name -eq 'Probe')
    Check ($probe.Count -eq 1 -and $probe[0].position.x -eq 2) 'Created node persists after a separate process reopens the scene'
    $null=Op @{op='scene_edit';scene='main.tscn';operations=@(@{op='create';name='Transient';type='Node3D'},@{op='remove';node='Missing'})} -FailureExpected
    $unchanged=Op @{op='scene_inspect';scene='main.tscn'}
    Check ($unchanged.sha256 -eq $reopened.sha256) 'A failed batch does not alter the saved scene'
    $null=Op @{op='scene_edit';scene='main.tscn';expected_sha256=$scene.sha256;operations=@()} -FailureExpected
    $null=Op @{op='undo';path=$reopened.file_path;expected_sha256=$reopened.sha256}
    $restored=Op @{op='scene_inspect';scene='main.tscn'}
    Check ($restored.sha256 -eq $scene.sha256) 'Undo restores the previous scene bytes'
    $null=Op @{op='write';path='../escape.gd';text='denied'} -FailureExpected
    Check (-not (Test-Path (Join-Path $work 'escape.gd'))) 'Path traversal is refused'
    $null=Op @{op='draft_save';path='godot/draft.gd';text='unsaved draft';base_sha256='';draft_id='acceptance'}
    $draft=Op @{op='draft_read';path='godot/draft.gd'}
    Check ($draft.draft.text -eq 'unsaved draft' -and -not (Test-Path (Join-Path $project 'godot/draft.gd'))) 'Draft survives a separate process without changing the source file'
    $null=Op @{op='draft_clear';path='godot/draft.gd';draft_id='acceptance'}
    $null=Op @{op='write';path='godot/probe.gd';expected_sha256='';text="extends MeshInstance3D`nfunc _ready() -> void:`n    print(`"AURUM_HEADLESS_GAMEPLAY_OK`")`n"}
    $null=Op @{op='scene_edit';scene='main.tscn';operations=@(@{op='attach_script';node='Cube';script='res://probe.gd'})}
    $validated=Op @{op='validate'}
    Check $validated.ok 'Saved scripts and scenes validate through Aurum'
    $play=Op @{op='play';frames=8}
    Check ($play.ok -and (($play.log -join "`n") -match 'AURUM_HEADLESS_GAMEPLAY_OK')) 'Bounded headless gameplay runs the attached script'
    $reportScript = @'
extends Node
func _ready() -> void:
    var args = OS.get_cmdline_user_args()
    var index = args.find("--aurum-report")
    if index >= 0:
        var file = FileAccess.open(args[index + 1], FileAccess.WRITE)
        file.store_string(JSON.stringify({"ok": not "--fail" in args, "marker": "dedicated scene", "argument": "two words" in args}))
        file.close()
    print("AURUM_DEDICATED_TEST_SCENE")
    get_tree().quit()
'@
    $null=Op @{op='write';path='godot/report_probe.gd';text=$reportScript;expected_sha256=''}
    $null=Op @{op='scene_create';scene='report_probe.tscn';root_type='Node';operations=@(@{op='attach_script';node='.';script='res://report_probe.gd'})}
    $report=Op @{op='play';scene='report_probe.tscn';fixed_fps=60;frames=10;user_args=@('two words');report=$true}
    Check ($report.ok -and $report.report.argument -and $report.report.marker -eq 'dedicated scene') 'Scene selection, spaced user arguments, and a fresh structured report work together'
    $failedReport=Op @{op='play';scene='report_probe.tscn';frames=10;user_args=@('--fail');report=$true} -FailureExpected
    Check (-not $failedReport.ok -and -not $failedReport.report.ok) 'A failing report fails play even when the engine exits zero'
    $missingReport=Op @{op='play';frames=2;report=$true} -FailureExpected
    Check (-not $missingReport.ok -and $missingReport.report_error) 'A missing report cannot reuse a prior passing result'
    $null=Op @{op='play';frames=0} -FailureExpected
    foreach($platform in @('Android','Web','Linux','macOS','iOS','Windows Desktop')){
        $configured=Op @{op='configure_export';platform=$platform}
        Check ($configured.ok -and $configured.platform -eq $platform) "Export preset configured for $platform"
        $before=(Op @{op='read';path='godot/export_presets.cfg'}).sha256
        $null=Op @{op='configure_export';platform=$platform}
        Check ((Op @{op='read';path='godot/export_presets.cfg'}).sha256 -eq $before) "Repeated $platform configuration preserves existing preset bytes"
    }
    $null=Op @{op='configure_export';platform='Invalid'} -FailureExpected
    $classes=Op @{op='classes';query='Node3D'}
    Check ($classes.classes -contains 'Node3D') 'Class discovery comes from the installed runtime'
    $property=Op @{op='class_info';class='Node3D'}
    Check (@($property.properties|Where-Object name -eq 'position').Count -eq 1) 'Property discovery returns the runtime schema'
    $null=Op @{op='scene_create';scene='alternate.tscn';root_type='Node3D';operations=@(@{op='instance';source='res://main.tscn';name='Level'})}
    $main=Op @{op='set_main_scene';scene='alternate.tscn'}
    Check ($main.ok -and $main.saved -and $main.file_path.EndsWith('project.godot')) 'Aurum sets the start scene through the runtime settings writer'
    $alternate=Op @{op='play';frames=8}
    Check ($alternate.ok -and (($alternate.log -join "`n") -match 'AURUM_HEADLESS_GAMEPLAY_OK')) 'Instanced content runs from the selected start scene'
    if($Package){
        $packagedGame=Op @{op='package';output='dist/acceptance'}
        Check ($packagedGame.ok -and (Test-Path $packagedGame.executable)) 'Windows package includes its runtime'
        $out=Join-Path $work 'packaged.stdout.log';$err=Join-Path $work 'packaged.stderr.log'
        $game=Start-Process -FilePath $packagedGame.executable -WorkingDirectory $packagedGame.directory -ArgumentList @('--headless','--quit-after','8') -WindowStyle Hidden -RedirectStandardOutput $out -RedirectStandardError $err -PassThru
        if(-not $game.WaitForExit(30000)){$game.Kill($true);throw 'Packaged game timed out'}
        Check ($game.ExitCode -eq 0 -and (Get-Content -Raw $out).Contains('AURUM_HEADLESS_GAMEPLAY_OK')) 'Packaged game runs without invoking an external engine'
    }

    $start=[Diagnostics.ProcessStartInfo]::new()
    $start.FileName=$AurumBinary
    $start.UseShellExecute=$false
    $start.CreateNoWindow=$true
    $start.RedirectStandardInput=$true
    $start.RedirectStandardOutput=$true
    $start.RedirectStandardError=$true
    foreach($arg in @('mcp','--root',$project,'--tools','studio','--read-only')){$start.ArgumentList.Add($arg)}
    $mcp=[Diagnostics.Process]::new();$mcp.StartInfo=$start;$null=$mcp.Start()
    $out=$mcp.StandardOutput.ReadToEndAsync();$err=$mcp.StandardError.ReadToEndAsync()
    $mcp.StandardInput.WriteLine('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"acceptance","version":"1"}}}')
    $mcp.StandardInput.WriteLine('{"jsonrpc":"2.0","id":2,"method":"tools/list"}')
    $mcp.StandardInput.WriteLine('{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"aurum_project_query","arguments":{"op":"scene_inspect","scene":"main.tscn"}}}')
    $mcp.StandardInput.WriteLine('{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"aurum_project_action","arguments":{"op":"write","path":"blocked.txt","text":"blocked"}}}')
    $mcp.StandardInput.Close()
    if(-not $mcp.WaitForExit(30000)){$mcp.Kill($true);throw 'MCP timed out'}
    $responses=@($out.Result -split "`n"|Where-Object{$_.Trim()}|ForEach-Object{$_|ConvertFrom-Json -Depth 100})
    $catalog=$responses|Where-Object id -eq 2
    Check ($catalog.result.tools.Count -eq 2) 'Read-only Studio profile has two tools'
    $inspection=$responses|Where-Object id -eq 3
    Check (-not $inspection.result.isError) 'A standard MCP client inspects the saved project headlessly'
    $denied=$responses|Where-Object id -eq 4
    Check ($null -ne $denied.error -and -not (Test-Path (Join-Path $project 'blocked.txt'))) 'Read-only MCP refuses mutation'
    $mcp.Dispose()

    $start=[Diagnostics.ProcessStartInfo]::new();$start.FileName=$AurumBinary;$start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    foreach($arg in @('studio',$project,'--no-open','--json')){$start.ArgumentList.Add($arg)}
    $server=[Diagnostics.Process]::new();$server.StartInfo=$start;$null=$server.Start()
    $line=$server.StandardOutput.ReadLineAsync();if(-not $line.Wait(10000)){throw 'Studio did not start'}
    $endpoint=$line.Result|ConvertFrom-Json
    $url=[Uri]$endpoint.url;$origin=$url.GetLeftPart([UriPartial]::Authority)
    $headers=@{'X-Aurum-Token'=($url.Query -replace '^\?t=','')}
    $page=Invoke-WebRequest $endpoint.url
    Check ($page.StatusCode -eq 200 -and $page.Content.Contains('workspace.js')) 'Studio serves the integrated workspace'
    $denied=Invoke-WebRequest "$origin/api/projects" -SkipHttpErrorCheck
    Check ($denied.StatusCode -eq 401) 'Project management requires authentication'
    $request=@{action='create';path=(Join-Path $work 'browser-created');name='browser-created';template='2d'}|ConvertTo-Json
    $created=Invoke-RestMethod "$origin/api/projects" -Method Post -Headers $headers -ContentType application/json -Body $request
    Check ($created.selected -eq (Join-Path $work 'browser-created')) 'Studio creates and selects a project'
    $request=@{op='scene_inspect';scene='main.tscn'}|ConvertTo-Json
    $inspected=Invoke-RestMethod "$origin/api/project" -Method Post -Headers $headers -ContentType application/json -Body $request
    Check ($inspected.tree.type -eq 'Node2D') 'Studio API operates on the selected project'
    $bound=@{op='read';path='godot/main.gd';project=$project}|ConvertTo-Json
    $original=Invoke-RestMethod "$origin/api/project" -Method Post -Headers $headers -ContentType application/json -Body $bound
    Check ($original.text.Contains('extends Node3D')) 'An explicit request keeps its project identity after Studio selection changes'
    $null=Invoke-RestMethod "$origin/api/stop" -Method Post -Headers $headers
    Check ($server.WaitForExit(10000)) 'Studio shuts down its owned service'
    @{passed=$true;checks=$checks;project=$project;work=$work}|ConvertTo-Json -Depth 8|Out-File (Join-Path $work 'evidence.json') -Encoding utf8
    "STUDIO_PROJECT_ACCEPTANCE_OK checks=$($checks.Count) evidence=$(Join-Path $work 'evidence.json')"
} finally {
    if($null -ne $server){if(-not $server.HasExited){$server.Kill($true);$server.WaitForExit()};$server.Dispose()}
}
