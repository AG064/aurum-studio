[CmdletBinding()]
param([Parameter(Mandatory)][string]$Destination,[string]$Toolchain='nightly-2026-09-30')
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$Destination=[IO.Path]::GetFullPath($Destination)
if($Destination -eq [IO.Path]::GetPathRoot($Destination)){throw 'Use a dedicated SDK directory, not a drive root'}
if($Toolchain -notmatch '^nightly(-\d{4}-\d{2}-\d{2})?$'){throw 'Choose nightly or a dated nightly toolchain'}
New-Item -ItemType Directory -Path $Destination -Force|Out-Null
$sdk=Join-Path $Destination 'emsdk'
if(-not (Test-Path -LiteralPath $sdk)){
    & git clone --depth 1 --branch 3.1.74 https://github.com/emscripten-core/emsdk.git $sdk *> (Join-Path $Destination 'sdk-clone.log')
    if($LASTEXITCODE){throw 'Pinned Emscripten checkout failed'}
}else{
    $remote=& git -C $sdk remote get-url origin
    $tag=& git -C $sdk describe --tags --exact-match
    if($LASTEXITCODE -or $remote -ne 'https://github.com/emscripten-core/emsdk.git' -or $tag -ne '3.1.74'){throw 'Existing SDK is not the expected pinned checkout'}
}
$emsdk=Join-Path $sdk $(if($IsWindows){'emsdk.bat'}else{'emsdk'})
& $emsdk install 3.1.74 *> (Join-Path $Destination 'sdk-install.log')
if($LASTEXITCODE){throw 'Emscripten installation failed; inspect sdk-install.log'}
& $emsdk activate 3.1.74 *> (Join-Path $Destination 'sdk-activate.log')
if($LASTEXITCODE){throw 'Emscripten activation failed'}
$previous=$env:RUSTUP_HOME
try{
    $env:RUSTUP_HOME=Join-Path $Destination 'rustup'
    & rustup toolchain install $Toolchain --profile minimal --component rust-src --target wasm32-unknown-emscripten --no-self-update *> (Join-Path $Destination 'rust-install.log')
    if($LASTEXITCODE){throw 'Isolated nightly installation failed; inspect rust-install.log'}
}finally{
    if($null -eq $previous){Remove-Item Env:RUSTUP_HOME -ErrorAction SilentlyContinue}else{$env:RUSTUP_HOME=$previous}
}
$result=@{ok=$true;emsdk=$sdk;rustup=(Join-Path $Destination 'rustup');toolchain=$Toolchain;emscripten='3.1.74';global_path_changed=$false}
$result|ConvertTo-Json|Out-File (Join-Path $Destination 'toolchain.json') -Encoding utf8
$result|ConvertTo-Json
