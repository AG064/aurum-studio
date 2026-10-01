[CmdletBinding()]
param([Parameter(Mandatory)][string]$GodotBinary)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$repo=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$GodotBinary=(Resolve-Path -LiteralPath $GodotBinary).Path
$work=Join-Path ([IO.Path]::GetTempPath()) ('aurum-runtime-acceptance-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work,(Join-Path $work 'userdata') | Out-Null
$bridge=Join-Path $repo 'crates/aurum-studio-core/src/runtime_bridge.gd'
$results=[ordered]@{work=$work}
function Run-Godot([string]$Name,[string[]]$Arguments,[int]$Budget){
    $info=[Diagnostics.ProcessStartInfo]::new($GodotBinary)
    $info.UseShellExecute=$false
    $info.CreateNoWindow=$true
    $info.RedirectStandardOutput=$true
    $info.RedirectStandardError=$true
    foreach($arg in $Arguments){$info.ArgumentList.Add($arg)}
    $info.Environment['APPDATA']=Join-Path $work 'userdata'
    $info.Environment['XDG_DATA_HOME']=Join-Path $work 'userdata'
    $process=[Diagnostics.Process]::Start($info)
    try{
        $stdout=$process.StandardOutput.ReadToEndAsync()
        $stderr=$process.StandardError.ReadToEndAsync()
        $timedOut=-not $process.WaitForExit($Budget*1000)
        if($timedOut){
            $process.Kill($true)
            $process.WaitForExit()
        }
        $text=$stdout.GetAwaiter().GetResult()
        $errors=$stderr.GetAwaiter().GetResult()
        $text | Out-File (Join-Path $work "$Name.log") -Encoding utf8
        $errors | Out-File (Join-Path $work "$Name-errors.log") -Encoding utf8
        if($timedOut){throw "$Name exceeded its $Budget second fixture budget; logs: $work"}
        if($process.ExitCode -ne 0 -or $errors.Contains('ERROR:')){throw "$Name failed; inspect $work"}
        return $text
    }finally{$process.Dispose()}
}
function Run-Fixture([string]$Name,[string]$Project,[string]$Script,[string]$Marker,[int]$MinimumChecks){
    $text=Run-Godot $Name @('--headless','--path',$Project,'--script',$Script,'--',$bridge) 30
    $line=@($text -split "`n" | Where-Object {$_.StartsWith($Marker+' ')})
    if($line.Count -ne 1){throw "$Name did not write exactly one acceptance result"}
    $receipt=$line[0].Substring($Marker.Length+1) | ConvertFrom-Json
    if(-not $receipt.ok -or $receipt.checks -lt $MinimumChecks){throw "$Name did not meet its acceptance contract"}
    $results[$Name]=$receipt
}
$ordinary=Join-Path $work 'ordinary'
Copy-Item -LiteralPath (Join-Path $repo 'scripts/tests/fixtures/live-preview/godot') -Destination $ordinary -Recurse
Run-Fixture 'runtime' $ordinary (Join-Path $PSScriptRoot 'runtime_bridge_acceptance.gd') 'AURUM_RUNTIME_ACCEPTANCE' 21
$orbit=Join-Path $work 'orbit'
New-Item -ItemType Directory -Path $orbit | Out-Null
$null=& robocopy (Join-Path $repo 'examples/orbit-break/godot') $orbit /E /XD .godot .aurum dist .git /NFL /NDL /NJH /NJS
if($LASTEXITCODE -ge 8){throw 'Could not create the disposable checkpoint fixture'}
$null=Run-Godot 'checkpoint-import' @('--headless','--editor','--path',$orbit,'--import') 120
Run-Fixture 'checkpoint' $orbit (Join-Path $PSScriptRoot 'orbit_checkpoint_acceptance.gd') 'AURUM_ORBIT_CHECKPOINT' 10
$results['ok']=$true
$results | ConvertTo-Json -Depth 10 | Out-File (Join-Path $work 'evidence.json') -Encoding utf8
$results | ConvertTo-Json -Depth 10
