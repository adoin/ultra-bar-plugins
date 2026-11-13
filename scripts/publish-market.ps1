[CmdletBinding()]
param([Parameter(Mandatory)][string]$Repository, [Parameter(Mandatory)][string]$PlanFile,
    [string]$OutputDirectory='target/market-publication')
. "$PSScriptRoot/market-common.ps1"
$plan = Get-Content -LiteralPath $PlanFile -Raw | ConvertFrom-Json -AsHashtable
if ($plan.revision -notmatch '^[a-f0-9]{40}$' -or $Repository -notmatch '^[\w.-]+/[\w.-]+$') { throw 'Invalid market source' }
$state = Get-PublishedState $Repository
if ($state.repository_id -ne $plan.repository_id) { throw 'Repository identity changed' }
$entries = @()
foreach ($manifest in $plan.manifests) {
    $entry = $state.plugins[$manifest.id]
    if (-not $entry -or $entry.version -cne $manifest.version) { throw "Expected published plugin not available: $($manifest.id) $($manifest.version)" }
    $entries += $entry
}
$catalog = @{schema_version=2;repository_id=$state.repository_id;revision=$plan.revision;plugins=@($entries | Sort-Object id)}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$output = Join-Path $OutputDirectory 'plugin-market.json'
$catalog | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $output -Encoding utf8NoBOM
$hash = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath "$output.sha256" -Value "$hash  plugin-market.json" -Encoding utf8NoBOM
$tag = "market/$($plan.revision)"
$null = & gh release view $tag --repo $Repository --json isDraft 2>$null
if ($LASTEXITCODE -eq 0) {
    $oldRelease = Read-ReleaseByTag $Repository $tag
    if (-not $oldRelease.draft) {
        $index = $oldRelease.assets | Where-Object name -CEQ 'plugin-market.json' | Select-Object -First 1
        if (-not $index) { throw 'Existing market snapshot is incomplete' }
        $old = Read-ReleaseCatalog $Repository $index
        if ($old.schema_version -ne 2 -or $old.repository_id -ne $catalog.repository_id -or $old.revision -cne $plan.revision -or @($old.plugins).Count -ne $entries.Count) { throw 'Existing market snapshot differs' }
        foreach ($entry in $entries) {
            $previous = @($old.plugins | Where-Object id -CEQ $entry.id)
            if ($previous.Count -ne 1 -or $previous[0].asset_id -ne $entry.asset_id -or $previous[0].sha256 -cne $entry.sha256 -or $previous[0].version -cne $entry.version) { throw 'Existing market asset reference differs' }
        }
        Write-Host "Market snapshot already published: $tag"
        exit 0
    }
    if ($oldRelease.target_commitish -cne $plan.revision) { throw 'Unpublished market draft belongs to another revision' }
    & gh release delete $tag --repo $Repository --yes
    if ($LASTEXITCODE) { throw 'Cannot reset incomplete market draft' }
}
$notes = Join-Path $OutputDirectory 'release-notes.md'
$lines = @('Official plugin market snapshot.', '', "Source: $($plan.revision)", '', "Built: $(@($plan.builds).Count); reused: $(@($plan.reuse).Count).", '')
$lines += $entries | ForEach-Object { "- $($_.name) $($_.version) [$($_.id)]" }
$lines | Set-Content -LiteralPath $notes -Encoding utf8NoBOM
Assert-TagTarget $Repository $tag $plan.revision
& gh release create $tag --repo $Repository --target $plan.revision --draft --latest=false --title "Plugin market $($plan.revision.Substring(0,7))" --notes-file $notes $output "$output.sha256"
if ($LASTEXITCODE) { throw 'Cannot upload market snapshot' }
& gh release edit $tag --repo $Repository --draft=false --latest=true
if ($LASTEXITCODE) { throw 'Cannot publish market snapshot' }
