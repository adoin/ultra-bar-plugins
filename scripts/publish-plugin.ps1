[CmdletBinding()]
param([Parameter(Mandatory)][string]$Repository, [Parameter(Mandatory)][string]$PluginId,
    [Parameter(Mandatory)][string]$Revision, [Parameter(Mandatory)][string]$PackageDirectory)
. "$PSScriptRoot/market-common.ps1"
if ($Repository -notmatch '^[\w.-]+/[\w.-]+$' -or $Revision -notmatch '^[a-f0-9]{40}$') { throw 'Invalid publication source' }
$catalog = Get-Content -LiteralPath (Join-Path $PackageDirectory 'plugin-catalog.json') -Raw | ConvertFrom-Json -AsHashtable
if ($catalog.schema_version -ne 1 -or @($catalog.plugins).Count -ne 1 -or $catalog.plugins[0].id -cne $PluginId) { throw 'Invalid package catalog' }
$entry = $catalog.plugins[0]
$null = Get-Version $entry.version
$tag = "$PluginId/v$($entry.version)"
$package = Join-Path $PackageDirectory $entry.asset
if ((Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant() -cne $entry.sha256) { throw 'Package checksum mismatch' }
$existing = & gh release view $tag --repo $Repository --json isDraft 2>$null
if ($LASTEXITCODE -eq 0) {
    $release = Read-ReleaseByTag $Repository $tag
    if (-not $release.draft) {
        $index = $release.assets | Where-Object name -CEQ 'plugin-catalog.json' | Select-Object -First 1
        if (-not $index) { throw 'Existing publication has no catalog' }
        $old = Read-ReleaseCatalog $Repository $index
        if ($old.schema_version -ne 1 -or @($old.plugins).Count -ne 1 -or $old.plugins[0].id -cne $PluginId -or $old.plugins[0].version -cne $entry.version) { throw 'Existing publication differs' }
        $null = Convert-ReleaseEntry $old.plugins[0] $release
        Write-Host "Retaining existing immutable package: $tag"
        exit 0
    }
    # A failed upload may leave our unpublished draft. Never delete a published release.
    if ($release.target_commitish -cne $Revision) { throw 'Unpublished draft belongs to another source revision' }
    & gh release delete $tag --repo $Repository --yes
    if ($LASTEXITCODE) { throw 'Cannot reset incomplete draft' }
}
$assets = @((Get-ChildItem -LiteralPath $PackageDirectory -File).FullName)
Assert-TagTarget $Repository $tag $Revision
& gh release create $tag --repo $Repository --target $Revision --draft --latest=false --title "$PluginId $($entry.version)" --notes "Plugin $PluginId $($entry.version). Source: $Revision." @assets
if ($LASTEXITCODE) { throw 'Cannot upload plugin release' }
$release = Read-ReleaseByTag $Repository $tag
$null = Convert-ReleaseEntry $entry $release
& gh release edit $tag --repo $Repository --draft=false --latest=false
if ($LASTEXITCODE) { throw 'Cannot publish plugin release' }
