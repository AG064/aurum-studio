[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$AurumBinary,
    [Parameter(Mandatory)][string]$Workspace,
    [Parameter(Mandatory)][string]$EditorDll,
    [Parameter(Mandatory)][string]$GodotBinary
)
$ErrorActionPreference='Stop'
$Workspace=(Resolve-Path $Workspace).Path
$tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')+'\'
if(-not ($Workspace+'\').StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)){
    throw 'This test requires a source copy under the temporary directory'
}
$AurumBinary=(Resolve-Path $AurumBinary).Path
$GodotBinary=(Resolve-Path $GodotBinary).Path
$id=[guid]::NewGuid().ToString('N')
$work=Join-Path $tempRoot "aurum-dev-acceptance-$id"
New-Item -ItemType Directory -Path $work,(Join-Path $work 'userdata'),(Join-Path $work 'state')|Out-Null
$env:AURUM_STUDIO_HOME=Join-Path $work 'state'
$env:APPDATA=Join-Path $work 'userdata'
$env:AURUM_GODOT=$GodotBinary
$env:CARGO_NET_OFFLINE='true'
Remove-Item Env:AURUM_RUNTIME_FINGERPRINT -ErrorAction SilentlyContinue
$project=Join-Path $Workspace 'godot'
Copy-Item -LiteralPath $EditorDll -Destination (Join-Path $project 'addons/aurum_editor/bin/aurum_editor.debug.dll') -Force
$source=Join-Path $Workspace 'crates/aurum-godot/src/build_info.rs'
$original=[IO.File]::ReadAllBytes($source)
$text=[Text.Encoding]::UTF8.GetString($original)
$fingerprint=Join-Path $project '.godot/aurum/live-fingerprint.txt'
$dll=Join-Path $project 'addons/aurum/bin/aurum_godot.debug.dll'
$editor=$null;$dev=$null;$observations=[Collections.Generic.List[string]]::new()
$success=$false;$editorPid=$null;$devPid=$null
function Fingerprint {
    if(Test-Path $fingerprint){try{return (Get-Content -Raw $fingerprint).Trim()}catch{}}
    return ''
}
function Wait-For([scriptblock]$Condition,[string]$Message,[int]$Seconds=180){
    $deadline=[DateTime]::UtcNow.AddSeconds($Seconds)
    while(-not (& $Condition)){
        if($null -ne $editor -and $editor.HasExited){throw 'Owned editor exited during the test'}
        if([DateTime]::UtcNow -gt $deadline){throw $Message}
        Start-Sleep -Milliseconds 200
    }
}
try {
    & $AurumBinary dev $Workspace --once --no-editor --json *> (Join-Path $work 'initial-build.log')
    if($LASTEXITCODE){throw 'Initial CLI build failed'}
    $editor=Start-Process -FilePath $GodotBinary -ArgumentList @('--headless','--editor','--path',$project) -WindowStyle Hidden -RedirectStandardOutput (Join-Path $work 'editor.stdout.log') -RedirectStandardError (Join-Path $work 'editor.stderr.log') -PassThru
    $editorPid=$editor.Id
    Wait-For { -not [string]::IsNullOrWhiteSpace((Fingerprint)) } 'No initial live fingerprint'
    $initial=Fingerprint;$observations.Add($initial)
    $devLog=Join-Path $work 'dev.stdout.log'
    $dev=Start-Process -FilePath $AurumBinary -ArgumentList @('dev',$Workspace,'--no-editor','--json') -WindowStyle Hidden -RedirectStandardOutput $devLog -RedirectStandardError (Join-Path $work 'dev.stderr.log') -PassThru
    $devPid=$dev.Id
    Wait-For { (Test-Path $devLog) -and (Get-Content -Raw $devLog) -match 'Develop is watching' } 'The shared supervisor did not start watching'
    [IO.File]::WriteAllText($source,$text+"`n// dev acceptance $id first`n",[Text.UTF8Encoding]::new($false))
    Wait-For { (Fingerprint) -ne $initial -and -not [string]::IsNullOrWhiteSpace((Fingerprint)) } 'A source change did not reach the running editor'
    $second=Fingerprint;$observations.Add($second)
    $workingHash=(Get-FileHash -LiteralPath $dll).Hash
    [IO.File]::WriteAllText($source,$text+"`ncompile_error!(`"intentional_aurum_dev_acceptance_failure`");`n",[Text.UTF8Encoding]::new($false))
    Wait-For { (Get-Content -Raw $devLog) -match 'build failed' } 'A failed build was not reported'
    if((Get-FileHash -LiteralPath $dll).Hash -ne $workingHash -or (Fingerprint) -ne $second){throw 'A failed build replaced the working runtime'}
    if($dev.HasExited){throw 'The watcher exited after a compiler error'}
    [IO.File]::WriteAllText($source,$text+"`n// dev acceptance $id recovered`n",[Text.UTF8Encoding]::new($false))
    Wait-For { (Fingerprint) -ne $second -and -not [string]::IsNullOrWhiteSpace((Fingerprint)) } 'The watcher did not recover after a failed build'
    $observations.Add((Fingerprint))
    $success=$true
} finally {
    if($null -ne $dev){if(-not $dev.HasExited){$dev.Kill($true);$dev.WaitForExit()};$dev.Dispose()}
    if($null -ne $editor){if(-not $editor.HasExited){$editor.Kill($true);$editor.WaitForExit()};$editor.Dispose()}
    [IO.File]::WriteAllBytes($source,$original)
}
if($success){
    if((Get-Process -Id $editorPid -ErrorAction SilentlyContinue) -or (Get-Process -Id $devPid -ErrorAction SilentlyContinue)){throw 'Owned processes remained after cleanup'}
    @{passed=$true;editor_pid=$editorPid;fingerprints=$observations;failed_build_preserved=$true;recovered=$true;cleanup_verified=$true;workspace=$Workspace}|ConvertTo-Json|Out-File (Join-Path $work 'evidence.json') -Encoding utf8
    "STUDIO_DEV_ACCEPTANCE_OK pid=$editorPid fingerprints=$($observations.Count) failed_build_preserved=true evidence=$(Join-Path $work 'evidence.json')"
}
