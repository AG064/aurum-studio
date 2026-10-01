[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Destination,
    [ValidateSet('Runtime','WebTemplates','All')][string]$Mode='All',
    [string]$TemplateArchive
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$Destination=[IO.Path]::GetFullPath($Destination)
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
$base='https://github.com/godotengine/godot-builds/releases/download/4.7-stable'
function Verified-Archive([string]$Name,[string]$Hash,[string]$Existing='') {
    $path=if($Existing){(Resolve-Path -LiteralPath $Existing).Path}else{Join-Path $Destination $Name}
    if(-not (Test-Path -LiteralPath $path)){
        $stage="$path.download"
        & curl.exe -L --fail --retry 2 --connect-timeout 30 --output $stage "$base/$Name"
        if($LASTEXITCODE){throw "Download failed: $Name"}
        if((Get-FileHash -LiteralPath $stage -Algorithm SHA256).Hash -ine $Hash){throw "Checksum mismatch: $Name"}
        [IO.File]::Move($stage,$path)
    }
    if((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $Hash){throw "Checksum mismatch: $Name"}
    return $path
}
function Extract-Entry($Zip,[string]$Entry,[string]$Target) {
    $item=$Zip.GetEntry($Entry)
    if(-not $item){throw "Expected archive entry is missing: $Entry"}
    New-Item -ItemType Directory -Path (Split-Path $Target -Parent) -Force | Out-Null
    $stage="$Target.stage.$([guid]::NewGuid().ToString('N'))"
    [IO.Compression.ZipFileExtensions]::ExtractToFile($item,$stage)
    if((Get-Item -LiteralPath $stage).Length -ne $item.Length){throw "Extracted size differs: $Entry"}
    [IO.File]::Move($stage,$Target,$true)
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$result=[ordered]@{version='4.7.stable';directory=$Destination}
if($Mode -in @('Runtime','All')){
    if(-not $IsWindows){throw 'The runtime provisioning mode currently targets Windows.'}
    $archive=Verified-Archive 'Godot_v4.7-stable_win64.exe.zip' '02a5312236f4e0209c78bcb2f52135b1963e6b8888c873c9cee81459e60bcd71'
    $zip=[IO.Compression.ZipFile]::OpenRead($archive)
    try {Extract-Entry $zip 'Godot_v4.7-stable_win64.exe' (Join-Path $Destination 'Godot_v4.7-stable_win64.exe')} finally {$zip.Dispose()}
    $result.godot_binary=Join-Path $Destination 'Godot_v4.7-stable_win64.exe'
}
if($Mode -in @('WebTemplates','All')){
    $archive=Verified-Archive 'Godot_v4.7-stable_export_templates.tpz' '9714459dc071907c0f3d5f17d608faf69e7cda21331fc5d39c4503ffa4e99eec' $TemplateArchive
    $zip=[IO.Compression.ZipFile]::OpenRead($archive)
    try {
        foreach($name in @('web_nothreads_release.zip','web_nothreads_debug.zip','web_dlink_nothreads_release.zip','web_dlink_nothreads_debug.zip')){
            Extract-Entry $zip "templates/$name" (Join-Path $Destination "templates/4.7.stable/$name")
        }
    } finally {$zip.Dispose()}
    $result.web_template=Join-Path $Destination 'templates/4.7.stable/web_nothreads_release.zip'
}
$result|ConvertTo-Json
