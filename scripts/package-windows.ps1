[CmdletBinding()]
param(
    # Use only after verify-windows.ps1 passed for these same sources/toolchain.
    [switch] $SkipChecks,
    # Stable names and metadata for an explicitly requested public release.
    [switch] $Release,
    # Optional machine-readable output for the installer and CI; never scrape console text.
    [string] $OutputManifestPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Packaging requires Windows x64 and the MSVC toolchain.' }
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ($OutputManifestPath) { $OutputManifestPath = [IO.Path]::GetFullPath($OutputManifestPath) }
Push-Location -LiteralPath $repositoryRoot
try {
    if (-not $SkipChecks) { & (Join-Path $PSScriptRoot 'verify-windows.ps1') }
    # Even with checks skipped, build the exact current sources; never copy an old EXE.
    & cargo build --release --locked --target x86_64-pc-windows-msvc | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    $metadataJson = & cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Cargo license inventory failed.' }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $project = $metadata.packages | Where-Object { $_.name -eq 'color-picker' }
    $commit = & git -c "safe.directory=$repositoryRoot" rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Could not identify source commit.' }
    $changes = @(& git -c "safe.directory=$repositoryRoot" status --porcelain)
    if ($LASTEXITCODE -ne 0) { throw 'Could not read source state.' }
    if ($Release -and $changes.Count -ne 0) { throw 'Release packaging requires a clean working tree.' }
    $packageName = if ($Release) {
        'color-picker-{0}-windows-x64' -f $project.version
    } else {
        'color-picker-{0}-windows-x64-preview-{1}-{2}' -f $project.version,
            (Get-Date -Format 'yyyyMMdd-HHmmss'), $commit.Substring(0, 7)
    }
    $dist = Join-Path $repositoryRoot 'dist'
    $package = Join-Path $dist $packageName
    # New unique output only. No recursive deletion and no overwriting older packages.
    New-Item -ItemType Directory -Path $package -ErrorAction Stop | Out-Null
    $exe = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc/release/color-picker.exe'
    Copy-Item -LiteralPath $exe -Destination (Join-Path $package 'color-picker.exe')
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE') -Destination $package
    # Keep both repository READMEs in full. Files omitted from the user package
    # remain accessible at the exact source revision; images use raw URLs.
    $sourceUrl = "https://github.com/ghostroller/color-picker/blob/$commit"
    $imageUrl = "https://raw.githubusercontent.com/ghostroller/color-picker/$commit"
    $localFiles = @('README.md', 'README.zh-CN.md', 'LICENSE')
    foreach ($name in @('README.md', 'README.zh-CN.md')) {
        $readme = Get-Content -LiteralPath (Join-Path $repositoryRoot $name) -Raw
        $readme = [regex]::Replace($readme, '(?<prefix>!?\[[^\]]*\]\()(?<target>[^)\s]+)(?<suffix>\))', {
            param($match)
            $target = $match.Groups['target'].Value
            $path = ($target -split '#', 2)[0]
            if ($target -match '^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|#)' -or $localFiles -contains $path) {
                return $match.Value
            }
            $base = if ($match.Groups['prefix'].Value.StartsWith('!')) { $imageUrl } else { $sourceUrl }
            return $match.Groups['prefix'].Value + $base + '/' + $target + $match.Groups['suffix'].Value
        })
        Set-Content -LiteralPath (Join-Path $package $name) -Value $readme -Encoding utf8 -NoNewline
    }
    $sysroot = & rustc --print sysroot
    if ($LASTEXITCODE -ne 0) { throw 'Could not locate Rust library notices.' }
    & (Join-Path $PSScriptRoot 'write-third-party-notices.ps1') -Metadata $metadata `
        -RustDocs (Join-Path $sysroot 'share/doc/rust') `
        -OutputPath (Join-Path $package 'THIRD-PARTY-NOTICES.html')
    $rustVersion = (& rustc -Vv) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Could not identify compiler.' }
    $info = [ordered]@{
        version = $project.version
        channel = $(if ($Release) { 'release' } else { 'local-preview' })
        source_commit = $commit
        source_dirty = ($changes.Count -ne 0)
        built_at_utc = [DateTime]::UtcNow.ToString('o')
        target = 'x86_64-pc-windows-msvc'
        rustc = $rustVersion
        os = [Environment]::OSVersion.VersionString
        crt = 'static'
        checks_run_by_packager = (-not $SkipChecks)
        exe_sha256 = (Get-FileHash -LiteralPath (Join-Path $package 'color-picker.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        exe_bytes = (Get-Item -LiteralPath (Join-Path $package 'color-picker.exe')).Length
    }
    # Build provenance is retained beside the archive for CI and maintainers.
    $buildInfo = Join-Path $dist "$packageName.build.json"
    $info | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $buildInfo -Encoding UTF8
    $zip = Join-Path $dist "$packageName.zip"
    Compress-Archive -LiteralPath $package -DestinationPath $zip -CompressionLevel Optimal
    $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$zip.sha256" -Value "$hash  $packageName.zip" -Encoding ASCII
    Write-Host "Package: $zip"
    Write-Host "SHA256:  $hash"
    if ($OutputManifestPath) {
        [ordered]@{
            version = $project.version
            source_commit = $commit
            source_dirty = ($changes.Count -ne 0)
            package_directory = $package
            build_info_path = $buildInfo
            archive_path = $zip
            archive_sha256_path = "$zip.sha256"
        } | ConvertTo-Json | Set-Content -LiteralPath $OutputManifestPath -Encoding UTF8
    }
}
finally { Pop-Location }
