[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Binary,
    [Parameter(Mandatory)][string]$GodotBinary,
    [Parameter(Mandatory)][string]$TemplatesDirectory,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$WindowsGameDirectory,
    [string]$WebGameDirectory
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$repo=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Binary=(Resolve-Path -LiteralPath $Binary).Path
$GodotBinary=(Resolve-Path -LiteralPath $GodotBinary).Path
$TemplatesDirectory=(Resolve-Path -LiteralPath $TemplatesDirectory).Path
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if($OutputDirectory.TrimEnd('\','/') -eq [IO.Path]::GetPathRoot($OutputDirectory).TrimEnd('\','/')){throw 'Use a new dedicated release directory, not a drive root'}
if(Test-Path -LiteralPath $OutputDirectory){throw 'Release output already exists; nothing was overwritten'}
$text=(& $Binary --version | Out-String).Trim()
if($LASTEXITCODE -or $text -notmatch '^aurum (\d+\.\d+\.\d+)$'){throw 'The release binary did not return a valid version'}
$version=$Matches[1]
$templates=@('web_nothreads_release.zip','web_nothreads_debug.zip','web_dlink_nothreads_release.zip','web_dlink_nothreads_debug.zip')
foreach($name in $templates){if(-not (Test-Path -LiteralPath (Join-Path $TemplatesDirectory $name) -PathType Leaf)){throw "Missing matching template: $name"}}
foreach($directory in @($WindowsGameDirectory,$WebGameDirectory)|Where-Object{$_}){
    if(-not (Test-Path -LiteralPath $directory -PathType Container)){throw "Game package directory does not exist: $directory"}
}
if($WebGameDirectory){
    $html=Get-Content -LiteralPath (Join-Path $WebGameDirectory 'index.html') -Raw
    if($html.Contains('aurum-preview.js')){throw 'Publish a standalone game, not an authenticated managed preview'}
}
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$studio=Join-Path $OutputDirectory 'Aurum Studio'
& (Join-Path $PSScriptRoot 'install.ps1') -Binary $Binary -GodotBinary $GodotBinary -StudioHome $studio -NoEnvironment -NoShortcut
if($LASTEXITCODE){throw 'Release installation failed'}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'release/Launch Aurum Studio.vbs') -Destination (Join-Path $studio 'Launch Aurum Studio.vbs') -Force
@{version=$text;binary='bin/aurum.exe';runtime=('runtime/'+(Split-Path $GodotBinary -Leaf));binary_sha256=(Get-FileHash -LiteralPath $Binary).Hash;runtime_sha256=(Get-FileHash -LiteralPath $GodotBinary).Hash;portable=$true}|ConvertTo-Json|Out-File (Join-Path $studio 'install.json') -Encoding utf8
$templateOutput=Join-Path $studio 'runtime/templates/4.7.stable'
New-Item -ItemType Directory -Path $templateOutput | Out-Null
foreach($name in $templates){
    $source=Join-Path $TemplatesDirectory $name
    $target=Join-Path $templateOutput $name
    Copy-Item -LiteralPath $source -Destination $target
    if((Get-FileHash -LiteralPath $source).Hash -ne (Get-FileHash -LiteralPath $target).Hash){throw "Template copy failed verification: $name"}
}
function Copy-SourceTree([string]$Source,[string]$Target){
    New-Item -ItemType Directory -Path $Target -Force | Out-Null
    $null=& robocopy $Source $Target /E /XD .git .godot .aurum target dist node_modules test-results playwright-report /XF '*.log' '*.import' /NFL /NDL /NJH /NJS
    if($LASTEXITCODE -ge 8){throw "Source copy failed: $Source"}
}
Copy-SourceTree (Join-Path $repo 'examples/orbit-break') (Join-Path $studio 'examples/orbit-break')
Copy-SourceTree (Join-Path $repo 'docs') (Join-Path $studio 'docs')
New-Item -ItemType Directory -Path (Join-Path $studio 'scripts') | Out-Null
foreach($name in @('provision-godot.ps1','provision-web-toolchain.ps1')){Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $studio "scripts/$name")}
Copy-Item -LiteralPath (Join-Path $repo 'README.md') -Destination (Join-Path $studio 'README.md')
$assets=[Collections.Generic.List[string]]::new()
function Archive([string]$Directory,[string]$Name){
    $archive=Join-Path $OutputDirectory $Name
    [IO.Compression.ZipFile]::CreateFromDirectory($Directory,$archive,[IO.Compression.CompressionLevel]::Optimal,$false)
    $assets.Add($archive)
}
Archive $studio "Aurum-Studio-$version-windows-x64.zip"
if($WindowsGameDirectory){
    if(-not (Test-Path -LiteralPath (Join-Path $WindowsGameDirectory 'game.exe')) -or -not (Test-Path -LiteralPath (Join-Path $WindowsGameDirectory 'game.pck'))){throw 'Windows game package needs game.exe and game.pck'}
    $game=Join-Path $OutputDirectory 'Orbit Break'
    Copy-SourceTree ([IO.Path]::GetFullPath($WindowsGameDirectory)) $game
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'release/Play Orbit Break.vbs') -Destination (Join-Path $game 'Play Orbit Break.vbs')
    Archive $game "Orbit-Break-$version-windows-x64.zip"
}
if($WebGameDirectory){Archive ([IO.Path]::GetFullPath($WebGameDirectory)) "Orbit-Break-$version-web.zip"}
$sums=@($assets|ForEach-Object{ '{0}  {1}' -f (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant(),(Split-Path $_ -Leaf) })
$sums|Out-File (Join-Path $OutputDirectory 'SHA256SUMS.txt') -Encoding utf8
@{ok=$true;version=$version;directory=$OutputDirectory;assets=$assets;binary_sha256=(Get-FileHash -LiteralPath $Binary).Hash;runtime_sha256=(Get-FileHash -LiteralPath $GodotBinary).Hash;global_environment_changed=$false}|ConvertTo-Json -Depth 6
