# Windows acceptance for real, offline Cargo job cancellation and deadlines.
# Each invocation preserves its unique temp fixture, logs and identity receipts.
param(
    [Parameter(Mandatory = $true)][string]$AurumBinary,
    [ValidateSet('Cancel','Deadline','Both')][string]$Mode = 'Both',
    [ValidateNotNullOrEmpty()][string]$TempRoot = [System.IO.Path]::GetTempPath()
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'PowerShell 7 is required' }
if (-not $IsWindows) { throw 'This witness requires Windows owned-worker support' }
$scratch = [System.IO.Path]::GetFullPath($TempRoot)
$binary = (Resolve-Path -LiteralPath $AurumBinary).Path
$binaryHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash
$run = Join-Path $scratch ('aurum-cargo-jobs-' + [guid]::NewGuid().ToString('N'))
$project = Join-Path $run 'project'
$marker = Join-Path $run 'build-script-marker.json'
$log = Join-Path $run 'harness.log'
$receipt = Join-Path $run 'receipt.json'
$utf8 = [System.Text.UTF8Encoding]::new($false)
$server = $null
$serverStderr = $null
$script:hostInfo = $null
$witness = $null
$owner = $null
$token = $null
$checks = [System.Collections.Generic.List[string]]::new()
$results = [System.Collections.Generic.List[object]]::new()
foreach ($directory in @($run,$project,(Join-Path $project 'src'),(Join-Path $project 'addons/native/bin'),(Join-Path $run 'tmp'))) {
    [System.IO.Directory]::CreateDirectory($directory) | Out-Null
}
function Save-Text([string]$Path,[string]$Text) { [System.IO.File]::WriteAllText($Path,$Text,$utf8) }
function Log([string]$Text) { [System.IO.File]::AppendAllText($log, $Text + [Environment]::NewLine,$utf8) }
function Check([bool]$Condition,[string]$Text) {
    if (-not $Condition) { throw $Text }
    $checks.Add($Text)
    Log $Text
}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CargoWitnessProcess {
    [StructLayout(LayoutKind.Sequential)] public struct FileTime { public uint Low; public uint High; }
    public sealed class Proof { public uint Pid; public string Created; public bool Gone; public int Error; }
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(IntPtr process, out FileTime created, out FileTime exited, out FileTime kernel, out FileTime user);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr process, uint milliseconds);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    public static Proof Inspect(uint pid) {
        var result = new Proof { Pid=pid };
        var handle = OpenProcess(0x1000 | 0x00100000, false, pid);
        if (handle == IntPtr.Zero) {
            result.Error=Marshal.GetLastWin32Error();
            result.Gone=result.Error==87 || result.Error==1168;
            return result;
        }
        try {
            FileTime created, exited, kernel, user;
            if (!GetProcessTimes(handle,out created,out exited,out kernel,out user)) { result.Error=Marshal.GetLastWin32Error(); return result; }
            result.Created="windows-filetime:"+(((ulong)created.High << 32) | created.Low).ToString(System.Globalization.CultureInfo.InvariantCulture);
            var wait=WaitForSingleObject(handle,0);
            result.Gone=wait==0;
            if (wait!=0 && wait!=258) result.Error=Marshal.GetLastWin32Error();
            return result;
        } finally { CloseHandle(handle); }
    }
}
'@
$configuration = @'
schema_version=1
name="cargo-job-witness"
rust_package="aurum-cargo-cancel-witness"
addon_destination="addons/native"
modules=[]
'@
$manifest = @'
[package]
name="aurum-cargo-cancel-witness"
version="0.1.0"
edition="2021"
build="build.rs"
[lib]
crate-type=["cdylib"]
[profile.dev]
debug=0
incremental=false
codegen-units=1
'@
$lock = @'
# Dependency-free, pre-pinned fixture. Cargo must preserve these exact bytes.
version = 4

[[package]]
name = "aurum-cargo-cancel-witness"
version = "0.1.0"
'@
$buildScript = @'
use std::ffi::c_void;
#[repr(C)] #[derive(Default)] struct FileTime { low:u32, high:u32 }
#[link(name="kernel32")]
extern "system" {
    fn GetCurrentProcess()->*mut c_void;
    fn GetProcessTimes(process:*mut c_void,created:*mut FileTime,exited:*mut FileTime,kernel:*mut FileTime,user:*mut FileTime)->i32;
}
fn main() {
    let (mut created,mut exited,mut kernel,mut user)=(FileTime::default(),FileTime::default(),FileTime::default(),FileTime::default());
    assert_ne!(unsafe {GetProcessTimes(GetCurrentProcess(),&mut created,&mut exited,&mut kernel,&mut user)},0);
    let ticks=((created.high as u64)<<32)|created.low as u64;
    let path=std::env::var_os("AURUM_BUILD_WITNESS_MARKER").expect("fixture marker environment");
    let temporary=std::path::PathBuf::from(&path).with_extension("tmp");
    std::fs::write(&temporary,format!("{{\"pid\":{},\"created\":\"windows-filetime:{}\"}}",std::process::id(),ticks)).unwrap();
    std::fs::rename(temporary,path).unwrap();
    println!("cargo:rerun-if-env-changed=AURUM_BUILD_WITNESS_MARKER");
    for _ in 0..600 { std::thread::sleep(std::time::Duration::from_millis(100)); }
}
'@
$extension = @'
[configuration]
entry_symbol="fixture_entry"
compatibility_minimum="4.7"
[libraries]
windows.debug.x86_64="./installed.debug.dll"
windows.release.x86_64="./installed.release.dll"
'@
Save-Text (Join-Path $project 'aurum.toml') $configuration
Save-Text (Join-Path $project 'Cargo.toml') $manifest
Save-Text (Join-Path $project 'Cargo.lock') $lock
Save-Text (Join-Path $project 'build.rs') $buildScript
Save-Text (Join-Path $project 'src/lib.rs') '#[no_mangle] pub extern "C" fn fixture_entry() -> u32 { 7 }'
Save-Text (Join-Path $project 'addons/native/bin/fixture.gdextension') $extension
Save-Text (Join-Path $project 'addons/native/bin/installed.debug.dll') 'installed-library-sentinel-must-survive'
$protected = @('aurum.toml','Cargo.toml','Cargo.lock','build.rs','src/lib.rs','addons/native/bin/fixture.gdextension','addons/native/bin/installed.debug.dll')
$before = @{}
foreach ($relative in $protected) { $before[$relative] = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $project $relative)).Hash }
$environment = @{
    AURUM_STUDIO_HOME=(Join-Path $run 'state'); CARGO_NET_OFFLINE='true'; CARGO_INCREMENTAL='0';
    CARGO_TARGET_DIR=(Join-Path $project 'target'); AURUM_BUILD_WITNESS_MARKER=$marker;
    APPDATA=(Join-Path $run 'userdata'); LOCALAPPDATA=(Join-Path $run 'userdata');
    TEMP=(Join-Path $run 'tmp'); TMP=(Join-Path $run 'tmp')
}
function Start-Child([string]$File,[string[]]$Arguments) {
    $start = [System.Diagnostics.ProcessStartInfo]::new($File)
    $start.UseShellExecute=$false; $start.CreateNoWindow=$true; $start.WindowStyle=[System.Diagnostics.ProcessWindowStyle]::Hidden
    $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    foreach ($key in $environment.Keys) { $start.Environment[$key]=[string]$environment[$key] }
    return [System.Diagnostics.Process]::Start($start)
}
function Api([hashtable]$Request) {
    $json = ConvertTo-Json -InputObject $Request -Depth 20 -Compress
    return Invoke-RestMethod -Method Post -Uri ("http://127.0.0.1:{0}/api/project" -f $script:hostInfo.port) -ContentType 'application/json' -Headers @{'X-Aurum-Token'=$token} -Body $json -TimeoutSec 30
}
function Wait-Until([scriptblock]$Probe,[scriptblock]$Accept,[string]$Label,[int]$Milliseconds=30000) {
    $until = [DateTime]::UtcNow.AddMilliseconds($Milliseconds)
    while ([DateTime]::UtcNow -lt $until) {
        $value = & $Probe
        if (& $Accept $value) { return $value }
        Start-Sleep -Milliseconds 50
    }
    throw ($Label + ' deadline exceeded')
}
try {
    Log ('Binary SHA256: ' + $binaryHash)
    $witness = Start-Child (Get-Process -Id $PID).Path @('-NoProfile','-NonInteractive','-Command','$until=[DateTime]::UtcNow.AddSeconds(120); while([DateTime]::UtcNow -lt $until){Start-Sleep -Milliseconds 100}')
    $witnessIdentity = [CargoWitnessProcess]::Inspect([uint32]$witness.Id)
    Check (-not $witnessIdentity.Gone -and $witnessIdentity.Error -eq 0) 'Unrelated witness is alive with native creation identity'
    $server = Start-Child $binary @('studio',$project,'--no-open','--json','--port','0')
    $serverStderr = $server.StandardError.ReadToEndAsync()
    $line = $server.StandardOutput.ReadLineAsync()
    if (-not $line.Wait(15000)) { throw 'Controller startup deadline exceeded' }
    $address = ConvertFrom-Json $line.Result
    $script:hostInfo = Get-Content -Raw -LiteralPath (Join-Path $project '.aurum/native/host.json') | ConvertFrom-Json
    $token = (Get-Content -Raw -LiteralPath (Join-Path $project '.aurum/native/host.token')).Trim()
    Check ($script:hostInfo.owner.pid -eq $server.Id) 'Fixture controller is the retained launched process'
    $variants = if ($Mode -eq 'Both') { @('Cancel','Deadline') } else { @($Mode) }
    foreach ($variant in $variants) {
        if (Test-Path -LiteralPath $marker) { Move-Item -LiteralPath $marker -Destination (Join-Path $run ('prior-marker-' + $results.Count + '.json')) }
        $seconds = if ($variant -eq 'Cancel') { 120 } else { 20 }
        $submission = @{op='job_submit'; request=@{op='build';release=$false}; timeout_seconds=$seconds}
        if ($owner) { $submission.owner_capability=$owner }
        $ticket = Api $submission
        $owner = $ticket.owner_capability
        Check ($ticket.ok -and $ticket.submitted) ($variant + ': real build admitted')
        Log ($variant + ': job ' + $ticket.job_id)
        $buildIdentity = Wait-Until {
            if (Test-Path -LiteralPath $marker) { return Get-Content -Raw -LiteralPath $marker | ConvertFrom-Json }
            $earlyStatus = Api @{op='job_status';owner_capability=$owner;job_id=$ticket.job_id}
            if ($earlyStatus.job.terminal) {
                $earlyResult = Api @{op='job_result';owner_capability=$owner;job_id=$ticket.job_id}
                $earlyJson = ConvertTo-Json -InputObject $earlyResult -Depth 20
                $earlyJson = $earlyJson.Replace($owner,'[owner omitted]').Replace($token,'[host token omitted]')
                Save-Text (Join-Path $run ($variant.ToLowerInvariant() + '-early-result.json')) $earlyJson
                throw ($variant + ' build finished before actual build-script marker; see retained early-result receipt')
            }
            return $null
        } { param($value) $null -ne $value -and $value.pid -gt 0 } ($variant + ' build-script marker') 60000
        $live = [CargoWitnessProcess]::Inspect([uint32]$buildIdentity.pid)
        Check ($live.Error -eq 0 -and -not $live.Gone -and $live.Created -eq $buildIdentity.created) ($variant + ': actual Cargo build-script creation identity observed alive')
        $control = @{owner_capability=$owner;job_id=$ticket.job_id}
        if ($variant -eq 'Cancel') {
            $cancel = Api (@{op='job_cancel'} + $control)
            Check ($cancel.ok -and $cancel.requested) 'Cancel: owner cancellation accepted'
        }
        $final = Wait-Until { Api (@{op='job_status'} + $control) } { param($value) $value.job.terminal } ($variant + ' terminal receipt') 30000
        $expected = if ($variant -eq 'Cancel') { 'cancelled' } else { 'timed_out' }
        Check ($final.job.state -eq $expected -and -not $final.job.committed -and $final.job.commit_status_verified -and -not $final.job.recovery_required) ($variant + ': clean terminal outcome with no publication')
        $retired = [CargoWitnessProcess]::Inspect([uint32]$buildIdentity.pid)
        Check ($retired.Gone -or ($retired.Error -eq 0 -and $retired.Created -ne $buildIdentity.created)) ($variant + ': recorded build-script process is gone')
        foreach ($relative in $protected) { Check ((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $project $relative)).Hash -eq $before[$relative]) ($variant + ': unchanged ' + $relative) }
        $unrelated = [CargoWitnessProcess]::Inspect([uint32]$witness.Id)
        Check ($unrelated.Error -eq 0 -and -not $unrelated.Gone -and $unrelated.Created -eq $witnessIdentity.Created) ($variant + ': unrelated process remains alive')
        $storedSummary = Get-Content -Raw -LiteralPath (Join-Path $project ('.aurum/background-jobs/job-' + $ticket.job_id + '.json'))
        Check (-not $storedSummary.Contains($owner) -and -not $storedSummary.Contains($token)) ($variant + ': persisted job summary excludes private capabilities')
        $row = [ordered]@{variant=$variant;job_id=$ticket.job_id;build_script=$buildIdentity;termination_proof=$retired;job=$final.job;protected_sha256=$before;unrelated_creation=$witnessIdentity.Created;unrelated_process=$unrelated}
        $rowJson = ConvertTo-Json -InputObject $row -Depth 20
        Check (-not $rowJson.Contains($owner) -and -not $rowJson.Contains($token)) ($variant + ': public receipt excludes private capabilities')
        $results.Add($row)
        Save-Text (Join-Path $run ($variant.ToLowerInvariant() + '-receipt.json')) $rowJson
        # A second variant runs only after the first passed every check. Failure
        # exits the loop and preserves its fixture; this is not an automatic retry.
    }
    Save-Text $receipt (ConvertTo-Json -InputObject ([ordered]@{ok=$true;checks=$checks.Count;run=$run;binary_sha256=$binaryHash;variants=$results;scope='real offline dependency-free Cargo process cancellation/deadline; no game, renderer or device proof'}) -Depth 25)
} catch {
    $message = $_.Exception.Message
    if ($owner) { $message=$message.Replace($owner,'[owner omitted]') }
    if ($token) { $message=$message.Replace($token,'[host token omitted]') }
    Log ('FAILED: ' + $message)
    Save-Text $receipt (ConvertTo-Json -InputObject ([ordered]@{ok=$false;checks=$checks.Count;run=$run;binary_sha256=$binaryHash;error=$message;completed_variants=$results}) -Depth 25)
    throw $message
} finally {
    if ($server) {
        if ($token -and $script:hostInfo) {
            try { Invoke-RestMethod -Method Post -Uri ("http://127.0.0.1:{0}/api/stop" -f $script:hostInfo.port) -Headers @{'X-Aurum-Token'=$token} -TimeoutSec 5 | Out-Null } catch {}
        }
        if (-not $server.WaitForExit(10000)) { $server.Kill(); $server.WaitForExit(10000) | Out-Null }
        if ($serverStderr) { Save-Text (Join-Path $run 'controller-stderr.log') $serverStderr.GetAwaiter().GetResult() }
        $server.Dispose()
    }
    if ($witness) {
        if (-not $witness.HasExited) { $witness.Kill(); $witness.WaitForExit(5000) | Out-Null }
        $witness.Dispose()
    }
    Write-Output ('Cargo job witness receipt: ' + $receipt)
}
