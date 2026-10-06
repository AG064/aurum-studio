[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$GodotBinary,
    [switch]$Offline,
    [switch]$NativeReload,
    [switch]$ReleaseInstall,
    [string]$ResumeDirectory
)
$ErrorActionPreference='Stop'
$repo=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$work=if($ResumeDirectory){[IO.Path]::GetFullPath($ResumeDirectory)}else{Join-Path ([IO.Path]::GetTempPath()) ('aurum-verification-'+[guid]::NewGuid().ToString('N'))}
$temporaryBase=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')+'\'
if(-not ($work+'\').StartsWith($temporaryBase,[StringComparison]::OrdinalIgnoreCase)){throw 'Verification work must stay under the temporary directory'}
$source=Join-Path $work 'source'
New-Item -ItemType Directory -Path $source,(Join-Path $work 'state') -Force|Out-Null
$files=@(& git -C $repo ls-files --cached --others --exclude-standard)
if($LASTEXITCODE){throw 'Could not enumerate the working tree'}
foreach($relative in $files){
    $from=Join-Path $repo $relative
    if(-not (Test-Path -LiteralPath $from -PathType Leaf)){continue}
    $to=[IO.Path]::GetFullPath((Join-Path $source $relative))
    if(-not $to.StartsWith($source+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Snapshot path escaped its root'}
    New-Item -ItemType Directory -Path (Split-Path $to -Parent) -Force|Out-Null
    Copy-Item -LiteralPath $from -Destination $to
}
$env:TEMP=$work;$env:TMP=$work
$env:AURUM_STUDIO_HOME=Join-Path $work 'state'
$env:CARGO_TARGET_DIR=Join-Path $work 'target'
$env:RUSTFLAGS='-D warnings'
if($Offline){$env:CARGO_NET_OFFLINE='true'}
$results=[Collections.Generic.List[object]]::new()
function Cargo-Gate([string]$Name,[string[]]$Arguments){
    $log=Join-Path $work "$Name.log"
    & cargo @Arguments *> $log
    $code=$LASTEXITCODE
    $results.Add(@{check=$Name;exit=$code;log=$log})
    Write-Host "$Name exit=$code log=$log"
    if($code){Get-Content $log -Tail 25;throw "$Name failed"}
}
Push-Location $source
try {
    Cargo-Gate 'format' @('fmt','--all','--','--check')
    Cargo-Gate 'tests' @('test','--workspace','--all-features','--locked')
    Cargo-Gate 'clippy' @('clippy','--workspace','--all-features','--all-targets','--locked','--','-D','warnings')
    Cargo-Gate 'build' @('build','-p','aurum-cli','-p','aurum-editor','--locked')
    $binary=Join-Path $env:CARGO_TARGET_DIR 'debug/aurum.exe'
    $editor=Join-Path $env:CARGO_TARGET_DIR 'debug/aurum_editor.dll'
    & pwsh -NoProfile -File (Join-Path $source 'scripts/tests/studio_project_acceptance.ps1') -AurumBinary $binary -GodotBinary $GodotBinary -Package
    if($LASTEXITCODE){throw 'Headless project acceptance failed'}
    & pwsh -NoProfile -File (Join-Path $source 'examples/relay-yard/tools/verify.ps1') -AurumBinary $binary -GodotBinary $GodotBinary -Package
    if($LASTEXITCODE){throw '3D reference workflow failed'}
    & pwsh -NoProfile -File (Join-Path $source 'scripts/tests/studio_editor_acceptance.ps1') -EditorDll $editor -GodotBinary $GodotBinary
    if($LASTEXITCODE){throw 'Live editor acceptance failed'}
    if($NativeReload){
        & pwsh -NoProfile -File (Join-Path $source 'scripts/tests/studio_dev_acceptance.ps1') -AurumBinary $binary -Workspace $source -EditorDll $editor -GodotBinary $GodotBinary
        if($LASTEXITCODE){throw 'Native development acceptance failed'}
    }
    if($ReleaseInstall){
        Cargo-Gate 'release' @('build','--release','-p','aurum-cli','--locked')
        & pwsh -NoProfile -File (Join-Path $source 'scripts/install.ps1') -Binary (Join-Path $env:CARGO_TARGET_DIR 'release/aurum.exe') -GodotBinary $GodotBinary -StudioHome (Join-Path $work 'installed') -NoEnvironment -NoShortcut
        if($LASTEXITCODE){throw 'Temporary installation failed'}
    }
    @{passed=$true;workspace=$work;source=$source;checks=$results}|ConvertTo-Json -Depth 8|Out-File (Join-Path $work 'verification.json') -Encoding utf8
    Write-Host "AURUM_STUDIO_VERIFICATION_OK evidence=$(Join-Path $work 'verification.json')"
} finally {Pop-Location}
