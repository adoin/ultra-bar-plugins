[CmdletBinding()]
param(
    [string]$PluginId = 'com.emssion.schedule-calendar',
    [string]$Tag = '',
    [string]$OutputDirectory = '',
    [string]$BinaryPath = '',
    [switch]$SkipBuild,
    [switch]$DebugBuild
)
$ErrorActionPreference = 'Stop'
$libraryRoot = Split-Path -Parent $PSScriptRoot
$sdkRoot = Join-Path (Split-Path -Parent $libraryRoot) 'ultra-bar'
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $libraryRoot 'dist' }
if ($DebugBuild -and [IO.Path]::GetFullPath($OutputDirectory) -eq [IO.Path]::GetFullPath((Join-Path $libraryRoot 'dist'))) { $OutputDirectory = Join-Path $libraryRoot 'target/package-debug' }
$candidates = @(Get-ChildItem -LiteralPath $libraryRoot -Directory | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'plugin.json') })
$directory = $null
foreach ($candidate in $candidates) { $data = Get-Content -LiteralPath (Join-Path $candidate.FullName 'plugin.json') -Raw | ConvertFrom-Json; if ($data.id -eq $PluginId) { if ($directory) { throw "Duplicate plugin id: $PluginId" }; $directory = $candidate } }
if (-not $directory) { throw "Plugin not found: $PluginId" }
$manifestPath = Join-Path $directory.FullName 'plugin.json'
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.id -notmatch '^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*){2,}$') { throw 'Published plugins require a stable reverse-DNS id.' }
$version = [string]$manifest.version
$semver = '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$'
if ($version -notmatch $semver) { throw 'plugin.json version must be SemVer.' }
if ($Tag -and $Tag -cne "$PluginId/v$version") { throw "Tag must exactly match $PluginId/v$version" }
$profile = if ($DebugBuild) { 'debug' } else { 'release' }
$binaryName = if ($manifest.panel.binary) { [string]$manifest.panel.binary } else { $directory.Name }
if ($binaryName -notmatch '^[a-z0-9_-]+$') { throw 'Invalid native binary name.' }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $libraryRoot 'target/release-build' }
if ($manifest.panel) {
    $cargoPath = Join-Path $directory.FullName 'Cargo.toml'
    $cargo = Get-Content -LiteralPath $cargoPath -Raw
    $nativeVersion = [regex]::Match($cargo,'(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
    if ($nativeVersion -ne $version) { throw 'Native Cargo version and plugin.json version differ.' }
    if (-not $SkipBuild) {
        $arguments = @('build','--manifest-path',$cargoPath,'--target-dir',$target,'--jobs','1')
        if (-not $DebugBuild) { $arguments += '--release' }
        if ($env:GITHUB_ACTIONS -eq 'true') { & cargo @arguments; if ($LASTEXITCODE -ne 0) { throw 'Native plugin build failed.' } }
        else {
            Add-Type -Path (Join-Path $sdkRoot 'scripts/DevProcess.cs')
            $code = [UltraBar.DevProcess]::Run((Get-Command cargo).Source,$arguments,$libraryRoot,20,2)
            if ($code -ne 0) { throw "Native plugin build failed: $code" }
        }
    }
    if (-not $BinaryPath) { $BinaryPath = Join-Path $target "$profile/$binaryName.exe" }
    if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) { throw "Missing native binary: $BinaryPath" }
    if ($SkipBuild -and -not $DebugBuild -and $BinaryPath -notmatch '[\\/]release[\\/]') { throw 'Release packages must use an actual release binary.' }
}

$files = [Collections.Generic.Dictionary[string,byte[]]]::new([StringComparer]::OrdinalIgnoreCase)
function Add-Payload([string]$Name,[byte[]]$Bytes) {
    if ($Name -match '(^/|\\|:|(^|/)\.\.(/|$))' -or $files.ContainsKey($Name)) { throw "Invalid/duplicate payload path: $Name" }
    $files.Add($Name,$Bytes)
}
Add-Payload 'plugin.json' ([IO.File]::ReadAllBytes($manifestPath))
$entry = [IO.Path]::GetFullPath((Join-Path $directory.FullName $manifest.entry))
$prefix = [IO.Path]::GetFullPath($directory.FullName).TrimEnd('\','/') + [IO.Path]::DirectorySeparatorChar
if (-not $entry.StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase)) { throw 'Rune entry escapes plugin directory.' }
foreach ($file in Get-ChildItem -LiteralPath $directory.FullName -File -Filter '*.rn') { Add-Payload $file.Name ([IO.File]::ReadAllBytes($file.FullName)) }
if (-not $files.ContainsKey([string]$manifest.entry)) { Add-Payload ([string]$manifest.entry).Replace('\','/') ([IO.File]::ReadAllBytes($entry)) }
$assets = Join-Path $directory.FullName 'assets'
if (Test-Path -LiteralPath $assets) { foreach ($file in Get-ChildItem -LiteralPath $assets -Recurse -File) { if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Symbolic links are not packaged.' }; Add-Payload ([IO.Path]::GetRelativePath($directory.FullName,$file.FullName).Replace('\','/')) ([IO.File]::ReadAllBytes($file.FullName)) } }
foreach ($file in Get-ChildItem -LiteralPath $directory.FullName -File | Where-Object { $_.Name -like 'LICENSE*' -or $_.Name -eq 'README.md' }) { Add-Payload $file.Name ([IO.File]::ReadAllBytes($file.FullName)) }
if ($manifest.panel) {
    if ($manifest.panel.runtime -cne '.runtime/current.json') { throw 'Native packages currently require .runtime/current.json.' }
    Add-Payload "bin/$binaryName.exe" ([IO.File]::ReadAllBytes($BinaryPath))
    Add-Payload '.runtime/current.json' ([Text.Encoding]::UTF8.GetBytes((@{executable="../bin/$binaryName.exe"}|ConvertTo-Json -Compress)))
}
function Revision([string]$Path) { $output = & git -C $Path rev-parse HEAD 2>$null; if ($LASTEXITCODE -ne 0) { return 'working-tree' }; return [string]$output }
$digests = @($files.Keys | Sort-Object | ForEach-Object { @{path=$_;sha256=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($files[$_])).ToLowerInvariant()} })
$metadata = @{format='ultra-bar-plugin';schema_version=1;id=$PluginId;name=$manifest.name;version=$version;platform='windows-x86_64';host='>=0.2.0,<1.0.0';files=$digests;build=@{profile=$profile;source_revision=(Revision $libraryRoot);sdk_revision=(Revision $sdkRoot)}}
$name = "$PluginId--$version--windows-x86_64$(if($DebugBuild){'.debug'}).ubplugin"
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$outputPath = Join-Path ([IO.Path]::GetFullPath($OutputDirectory)) $name
$stream = [IO.File]::Open($outputPath,[IO.FileMode]::Create,[IO.FileAccess]::ReadWrite)
$zip = [IO.Compression.ZipArchive]::new($stream,[IO.Compression.ZipArchiveMode]::Create,$false)
try {
    foreach ($path in @('package.json') + @($files.Keys | Sort-Object | ForEach-Object { "plugin/$_" })) {
        $bytes = if ($path -eq 'package.json') { [Text.Encoding]::UTF8.GetBytes(($metadata|ConvertTo-Json -Depth 12 -Compress)) } else { $files[$path.Substring(7)] }
        $item = $zip.CreateEntry($path,[IO.Compression.CompressionLevel]::Optimal)
        $writer = $item.Open(); try { $writer.Write($bytes,0,$bytes.Length) } finally { $writer.Dispose() }
    }
} finally { $zip.Dispose(); $stream.Dispose() }
$hash = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$outputPath.sha256","$hash  $name`n")
$catalog = @{schema_version=1;plugins=@(@{id=$PluginId;name=$manifest.name;version=$version;platform='windows-x86_64';host='>=0.2.0,<1.0.0';asset=$name;sha256=$hash;description=[string]$manifest.name})}
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'plugin-catalog.json'),($catalog|ConvertTo-Json -Depth 8))
Write-Host "Package: $outputPath"
Write-Host "SHA-256: $hash"
