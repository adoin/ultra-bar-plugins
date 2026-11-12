[CmdletBinding()]
param([Parameter(Mandatory)][string]$Repository, [string]$OutputDirectory='target/market')
. "$PSScriptRoot/market-common.ps1"
if ($Repository -notmatch '^[\w.-]+/[\w.-]+$') { throw 'Invalid repository' }
$root = Split-Path -Parent $PSScriptRoot
$state = Get-PublishedState $Repository
$manifests = @(Get-PluginManifests $root)
$plan = New-MarketPlan $manifests $state.plugins
$revision = (& git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE) { throw 'Cannot resolve source revision' }
$sdk = (Get-Content "$root/sdk-revision.txt" -Raw).Trim()
if ($sdk -notmatch '^[a-f0-9]{40}$') { throw 'Invalid SDK revision' }
$plan.repository_id = $state.repository_id
$plan.revision = $revision
$plan.sdk_revision = $sdk
$plan.manifests = $manifests
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$plan | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'plan.json') -Encoding utf8NoBOM
foreach ($entry in $plan.reuse) {
    $manifest = $manifests | Where-Object id -CEQ $entry.id | Select-Object -First 1
    & git -C $root rev-parse --verify --quiet "$($entry.release_tag)^{commit}" *> $null
    if ($LASTEXITCODE -eq 0) {
        & git -C $root diff --quiet $entry.release_tag HEAD -- $manifest.directory sdk-revision.txt
        if ($LASTEXITCODE -eq 1) { Write-Warning "$($entry.id): source/SDK changed without a version bump; retaining the published package." }
        elseif ($LASTEXITCODE) { throw 'Cannot compare published source' }
    }
    Write-Host "Reuse $($entry.id) $($entry.version): $($entry.download_url)"
}
foreach ($entry in $plan.builds) { Write-Host "Build $($entry.id) $($entry.version)" }
if ($env:GITHUB_OUTPUT) {
    "has_builds=$($plan.builds.Count -gt 0)".ToLowerInvariant() >> $env:GITHUB_OUTPUT
    "matrix=$(@{include=@($plan.builds)} | ConvertTo-Json -Depth 8 -Compress)" >> $env:GITHUB_OUTPUT
    "sdk_revision=$sdk" >> $env:GITHUB_OUTPUT
}
