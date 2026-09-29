[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$EditorDll,
    [Parameter(Mandatory)][string]$GodotBinary
)
$ErrorActionPreference='Stop'
$repo=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$work=Join-Path ([IO.Path]::GetTempPath()) ('aurum-editor-acceptance-'+[guid]::NewGuid().ToString('N'))
$addon=Join-Path $work 'addons/aurum_editor'
New-Item -ItemType Directory -Path $addon,(Join-Path $work 'userdata') -Force|Out-Null
Copy-Item (Join-Path $repo 'godot/addons/aurum_editor/*') $addon -Recurse -Force
Copy-Item -LiteralPath $EditorDll -Destination (Join-Path $addon 'bin/aurum_editor.debug.dll') -Force
[IO.File]::WriteAllText((Join-Path $work 'project.godot'),"config_version=5`n[application]`nconfig/name=`"Aurum editor acceptance`"`nrun/main_scene=`"res://main.tscn`"`n[editor_plugins]`nenabled=PackedStringArray(`"res://addons/aurum_editor/plugin.cfg`")`n[rendering]`nrenderer/rendering_method=`"gl_compatibility`"`n")
[IO.File]::WriteAllText((Join-Path $work 'main.tscn'),"[gd_scene format=3]`n[node name=`"Main`" type=`"Node3D`"]`n")
$start=[Diagnostics.ProcessStartInfo]::new();$start.FileName=(Resolve-Path $GodotBinary).Path;$start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
$start.Environment['APPDATA']=(Join-Path $work 'userdata')
foreach($arg in @('--headless','--editor','--path',$work,'res://main.tscn')){$start.ArgumentList.Add($arg)}
$process=[Diagnostics.Process]::new();$process.StartInfo=$start;$null=$process.Start();$stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
$bridge=Join-Path $work '.godot/aurum/editor'
function Request($Body) {
    $id=[guid]::NewGuid().ToString('N')
    $request=Join-Path $bridge "requests/$id.json"
    $response=Join-Path $bridge "responses/$id.json"
    $stage="$request.tmp"
    [IO.File]::WriteAllText($stage,($Body|ConvertTo-Json -Compress -Depth 10))
    [IO.File]::Move($stage,$request)
    $deadline=[DateTime]::UtcNow.AddSeconds(15)
    while(-not (Test-Path -LiteralPath $response)){
        if($process.HasExited -or [DateTime]::UtcNow -gt $deadline){throw 'Editor request did not complete'}
        Start-Sleep -Milliseconds 80
    }
    $value=Get-Content -Raw -LiteralPath $response|ConvertFrom-Json -Depth 20
    if(-not $value.ok){throw ($value|ConvertTo-Json -Depth 20)}
    return $value
}
try {
    $deadline=[DateTime]::UtcNow.AddSeconds(60)
    while(-not (Test-Path (Join-Path $bridge 'requests'))){
        if($process.HasExited -or [DateTime]::UtcNow -gt $deadline){throw 'Editor bridge did not start'}
        Start-Sleep -Milliseconds 100
    }
    $null=Request @{op='create_node';parent='.';type='Node3D';name='SavedByAgent'}
    $null=Request @{op='save_scene'}
    if(-not (Get-Content -Raw (Join-Path $work 'main.tscn')).Contains('SavedByAgent')){throw 'Created node was not saved'}
    $null=Request @{op='undo'}
    $null=Request @{op='save_scene'}
    if((Get-Content -Raw (Join-Path $work 'main.tscn')).Contains('SavedByAgent')){throw 'Undo did not remove the saved node'}
    $null=Request @{op='redo'}
    $null=Request @{op='save_scene'}
    if(-not (Get-Content -Raw (Join-Path $work 'main.tscn')).Contains('SavedByAgent')){throw 'Redo did not restore the saved node'}
    "STUDIO_EDITOR_ACCEPTANCE_OK create_save_undo_redo=true project=$work"
} finally {
    if(-not $process.HasExited){$process.Kill($true);$process.WaitForExit()}
    $stdout.Result|Out-File (Join-Path $work 'stdout.log') -Encoding utf8
    $stderr.Result|Out-File (Join-Path $work 'stderr.log') -Encoding utf8
    $process.Dispose()
    "EDITOR_TEST_LOGS=$work"
}
