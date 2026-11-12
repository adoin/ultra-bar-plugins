$ErrorActionPreference = 'Stop'
function Read-GitHubJson([string]$Endpoint) {
    $raw = & gh api $Endpoint
    if ($LASTEXITCODE) { throw "GitHub request failed: $Endpoint" }
    $raw | ConvertFrom-Json -AsHashtable
}
function Read-ReleaseCatalog([string]$Repository, $Asset) {
    if ($Asset.size -gt 2MB) { throw 'Release catalog exceeds 2 MiB' }
    $raw = & gh api "repos/$Repository/releases/assets/$($Asset.id)" -H 'Accept: application/octet-stream'
    if ($LASTEXITCODE) { throw 'Cannot read release catalog' }
    $raw | ConvertFrom-Json -AsHashtable
}
function Get-Version([string]$Version) {
    if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:[-+].+)?$') { throw "Invalid SemVer: $Version" }
    [System.Management.Automation.SemanticVersion]::Parse($Version)
}
function Get-PluginManifests([string]$Root) {
    $seen = @{}
    foreach ($directory in Get-ChildItem -LiteralPath $Root -Directory | Sort-Object Name) {
        $path = Join-Path $directory.FullName 'plugin.json'
        if (-not (Test-Path -LiteralPath $path)) { continue }
        $manifest = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json -AsHashtable
        if ($manifest.id -notmatch '^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*){2,}$' -or $seen.ContainsKey($manifest.id)) { throw 'Invalid or duplicate plugin id' }
        $seen[$manifest.id] = $true
        $version = Get-Version $manifest.version
        if ($version.PreReleaseLabel) { throw 'The stable market requires stable plugin versions' }
        $cargo = Join-Path $directory.FullName 'Cargo.toml'
        if (Test-Path -LiteralPath $cargo) {
            $toml = Get-Content -LiteralPath $cargo -Raw
            if ($toml -notmatch '(?ms)^\[package\]\s*(?:(?!^\[).)*?^version\s*=\s*"([^"]+)"' -or $Matches[1] -cne $manifest.version) { throw "Cargo.toml and plugin.json versions differ: $($manifest.id)" }
        }
        @{id=$manifest.id;version=$manifest.version;directory=$directory.Name;tag="$($manifest.id)/v$($manifest.version)"}
    }
}
function Convert-ReleaseEntry($Entry, $Release) {
    if ($Release.tag_name -cne "$($Entry.id)/v$($Entry.version)") { throw 'Release tag and plugin catalog differ' }
    $asset = @($Release.assets | Where-Object name -CEQ $Entry.asset)
    if ($asset.Count -ne 1 -or $asset[0].size -le 0 -or $asset[0].size -gt 256MB -or $Entry.sha256 -notmatch '^[a-fA-F0-9]{64}$') { throw 'Invalid plugin release asset' }
    if ($asset[0].digest -and $asset[0].digest -cne "sha256:$($Entry.sha256)") { throw 'GitHub asset digest and package catalog differ' }
    $result = @{}
    foreach ($key in @('id','name','version','platform','host','asset','sha256','description')) { $result[$key] = $Entry[$key] }
    $result.description = [string]$Entry.description
    $result.asset_id = $asset[0].id
    $result.download_url = $asset[0].browser_download_url
    $result.size = $asset[0].size
    $result.release_tag = $Release.tag_name
    $result
}
function Assert-TagTarget([string]$Repository, [string]$Tag, [string]$Revision) {
    $raw = & gh api "repos/$Repository/git/ref/tags/$Tag" 2>$null
    if ($LASTEXITCODE) { return } # Missing refs are created with --target by release create.
    $object = ($raw | ConvertFrom-Json -AsHashtable).object
    for ($i=0; $object.type -eq 'tag' -and $i -lt 4; $i++) {
        $object = (Read-GitHubJson "repos/$Repository/git/tags/$($object.sha)").object
    }
    if ($object.type -ne 'commit' -or $object.sha -cne $Revision) { throw 'Existing tag points to another source revision' }
}
function Get-PublishedState([string]$Repository) {
    $identity = Read-GitHubJson "repos/$Repository"
    $raw = & gh api --paginate --slurp "repos/$Repository/releases?per_page=100"
    if ($LASTEXITCODE) { throw 'Cannot list published releases' }
    $releases = @($raw | ConvertFrom-Json -AsHashtable | ForEach-Object { $_ } | Where-Object { -not $_.draft -and -not $_.prerelease })
    $latest = @{}
    foreach ($release in $releases) {
        if ($release.tag_name -notmatch '^([a-z][a-z0-9.-]+)/v(.+)$') { continue }
        $id = $Matches[1]; $version = $Matches[2]
        $parsed = Get-Version $version
        if ($parsed.PreReleaseLabel) { continue }
        if ($latest.ContainsKey($id) -and (Get-Version $latest[$id].version).CompareTo($parsed) -ge 0) { continue }
        $asset = $release.assets | Where-Object name -CEQ 'plugin-catalog.json' | Select-Object -First 1
        if (-not $asset) { continue }
        $catalog = Read-ReleaseCatalog $Repository $asset
        if ($catalog.schema_version -ne 1) { throw 'Unsupported package catalog schema' }
        $entry = @($catalog.plugins | Where-Object { $_.id -ceq $id -and $_.version -ceq $version })
        if ($entry.Count -ne 1) { throw "Release does not describe its own plugin: $id" }
        $latest[$id] = Convert-ReleaseEntry $entry[0] $release
    }
    @{repository_id=$identity.id;plugins=$latest}
}
function New-MarketPlan($Manifests, [hashtable]$Published) {
    $builds = @(); $reuse = @(); $seen = @{}
    foreach ($manifest in $Manifests) {
        if ($seen.ContainsKey($manifest.id)) { throw 'Duplicate plugin id' }
        $seen[$manifest.id] = $true
        $version = Get-Version $manifest.version
        $old = $Published[$manifest.id]
        if (-not $old) { $builds += $manifest; continue }
        $order = $version.CompareTo((Get-Version $old.version))
        if ($order -lt 0) { throw "Version downgrade: $($manifest.id) $($manifest.version) < $($old.version)" }
        if ($order -eq 0) {
            if ($manifest.version -cne $old.version) { throw 'Build metadata alone does not advance SemVer precedence' }
            $reuse += $old
        } else { $builds += $manifest }
    }
    @{builds=@($builds);reuse=@($reuse)}
}
