[CmdletBinding()]
param([Parameter(Mandatory)][string]$TestRoot)
. "$PSScriptRoot/market-common.ps1"
if (-not [IO.Path]::IsPathFullyQualified($TestRoot)) { throw 'Tests require an isolated absolute path' }
New-Item -ItemType Directory -Force $TestRoot | Out-Null
function Assert($Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-Fails([scriptblock]$Action) {
    $failed = $false
    try { & $Action } catch { $failed = $true }
    Assert $failed 'Expected validation failure'
}
$ids = @('com.example.agent','com.example.search','com.example.calendar')
$manifests = @($ids | ForEach-Object { @{id=$_;version='1.0.0';directory=($_ -split '\.')[-1];tag="$_/v1.0.0"} })
$published = @{}
foreach ($id in $ids) { $published[$id] = @{id=$id;version='1.0.0';download_url="https://github.com/example/plugins/releases/download/$id/v1.0.0/$id.ubplugin";sha256=('a'*64);asset_id=1} }
$unchanged = New-MarketPlan $manifests $published
Assert ($unchanged.builds.Count -eq 0 -and $unchanged.reuse.Count -eq 3) 'Unchanged versions must not compile'
$manifests[0].version = '1.0.1'; $manifests[0].tag = "$($ids[0])/v1.0.1"
$changed = New-MarketPlan $manifests $published
Assert ($changed.builds.Count -eq 1 -and $changed.builds[0].id -ceq $ids[0] -and $changed.reuse.Count -eq 2) 'Only the bumped plugin must compile'
Assert ($changed.reuse[0].download_url -ceq $published[$ids[1]].download_url) 'Unchanged package URL must be preserved'
# A previous attempt may have published the binary but failed before the market snapshot.
$published[$ids[0]] = @{id=$ids[0];version='1.0.1';download_url='original-successful-package-url';asset_id=2}
$retry = New-MarketPlan $manifests $published
Assert ($retry.builds.Count -eq 0 -and $retry.reuse[0].asset_id -eq 2) 'Successful packages must survive a catalog retry without recompilation'
$manifests[0].version='0.9.0'
Assert-Fails { New-MarketPlan $manifests $published }
$manifests[0].version='1.0.1+metadata'
Assert-Fails { New-MarketPlan $manifests $published }
$manifests[0].version='1.0.1'
Assert-Fails { New-MarketPlan @($manifests[0],$manifests[0]) $published }
$first = New-MarketPlan $manifests @{}
Assert ($first.builds.Count -eq 3) 'New plugins must build'
$fixture = Join-Path $TestRoot 'manifest-check'
New-Item -ItemType Directory -Force "$fixture/example" | Out-Null
'{"id":"com.example.test","version":"1.2.3"}' | Set-Content "$fixture/example/plugin.json"
@'
[package]
name = "example"
version = "1.2.3"
[dependencies]
'@ | Set-Content "$fixture/example/Cargo.toml"
Assert (@(Get-PluginManifests $fixture).Count -eq 1) 'Valid manifest must be accepted'
(Get-Content "$fixture/example/Cargo.toml" -Raw).Replace('1.2.3','1.2.4') | Set-Content "$fixture/example/Cargo.toml"
Assert-Fails { Get-PluginManifests $fixture }
Write-Host 'Market tests passed: version selection, immutable reuse, failed-catalog recovery, downgrade rejection, Cargo version validation.'
