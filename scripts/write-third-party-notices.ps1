[CmdletBinding()]
param(
    # The parsed result of cargo metadata for the exact locked build.
    [Parameter(Mandatory = $true)] [object] $Metadata,
    # The toolchain's share/doc/rust directory, including COPYRIGHT-library.html.
    [Parameter(Mandatory = $true)] [string] $RustDocs,
    # A new file in an existing directory; existing files are never overwritten.
    [Parameter(Mandatory = $true)] [string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$utf8 = [Text.UTF8Encoding]::new($false, $true)
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$output = [IO.Path]::GetFullPath($OutputPath)
if (Test-Path -LiteralPath $output) { throw 'The third-party notices output already exists.' }
if (-not (Test-Path -LiteralPath (Split-Path -Parent $output) -PathType Container)) {
    throw 'The third-party notices output directory must already exist.'
}
if (-not $Metadata.PSObject.Properties['packages'] -or @($Metadata.packages).Count -lt 2) {
    throw 'Cargo metadata must contain the project and its dependency packages.'
}
if (@($Metadata.packages | Where-Object { $_.name -eq 'color-picker' }).Count -ne 1) {
    throw 'Cargo metadata must contain exactly one color-picker package.'
}
$rustDirectory = (Get-Item -LiteralPath $RustDocs -ErrorAction Stop)
if (-not $rustDirectory.PSIsContainer) { throw 'RustDocs must be a directory.' }

function ConvertTo-HtmlText([string] $Text) {
    return [Net.WebUtility]::HtmlEncode($Text)
}

function Get-RelativeSourceName([string] $Root, [string] $Path) {
    $prefix = [IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('\', '/')) + [IO.Path]::DirectorySeparatorChar
    $full = [IO.Path]::GetFullPath($Path)
    if (-not $full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'A license source is outside its declared source directory.'
    }
    return $full.Substring($prefix.Length).Replace('\', '/')
}

function Read-Notice([string] $Path, [string] $LogicalName) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Missing notice source: $LogicalName."
    }
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -eq 0) { throw "Empty notice source: $LogicalName." }
    try { $contents = $utf8.GetString($bytes) }
    catch { throw "Notice source is not valid UTF-8: $LogicalName." }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $hash = [BitConverter]::ToString($sha256.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose() }
    return [pscustomobject]@{ Name = $LogicalName; Hash = $hash; Text = $contents }
}

$sources = [Collections.Generic.List[object]]::new()
$packages = [Collections.Generic.List[object]]::new()
foreach ($dependency in ($Metadata.packages | Where-Object { $_.name -ne 'color-picker' } | Sort-Object name, version)) {
    foreach ($property in @('name', 'version', 'manifest_path', 'license', 'license_file')) {
        if (-not $dependency.PSObject.Properties[$property]) {
            throw "Cargo dependency metadata is missing '$property'."
        }
    }
    if ($dependency.name -notmatch '^[A-Za-z0-9_-]+$' -or $dependency.version -notmatch '^[A-Za-z0-9.+_-]+$') {
        throw 'Cargo dependency name or version is invalid.'
    }
    $sourceDirectory = Split-Path -Parent $dependency.manifest_path
    if (-not (Test-Path -LiteralPath $dependency.manifest_path -PathType Leaf)) {
        throw "Missing manifest for $($dependency.name) $($dependency.version)."
    }
    # Match the previous packaging inventory, including build dependencies and
    # every alternative license. Do not infer a smaller legal scope here.
    $licenseFiles = @(Get-ChildItem -LiteralPath $sourceDirectory -File |
        Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE)' })
    if ($dependency.license_file) {
        $declaredFile = Join-Path $sourceDirectory $dependency.license_file
        $null = Get-RelativeSourceName $sourceDirectory $declaredFile
        $licenseFiles += Get-Item -LiteralPath $declaredFile -ErrorAction Stop
    }
    $licenseFiles = @($licenseFiles | Sort-Object FullName -Unique)
    if ($licenseFiles.Count -eq 0) { throw "Missing license text for $($dependency.name) $($dependency.version)." }
    $packageSources = [Collections.Generic.List[object]]::new()
    foreach ($file in $licenseFiles) {
        $relative = Get-RelativeSourceName $sourceDirectory $file.FullName
        $notice = Read-Notice $file.FullName "crates/$($dependency.name)-$($dependency.version)/$relative"
        $sources.Add($notice)
        $packageSources.Add($notice)
    }
    $packages.Add([pscustomobject]@{
        Name = $dependency.name
        Version = $dependency.version
        License = $dependency.license
        Sources = $packageSources
    })
}

$rustCopyright = Read-Notice (Join-Path $rustDirectory.FullName 'COPYRIGHT-library.html') 'rust/COPYRIGHT-library.html'
$rustLicenses = Join-Path $rustDirectory.FullName 'licenses'
$rustFiles = @(Get-ChildItem -LiteralPath $rustLicenses -Recurse -File | Sort-Object FullName)
if ($rustFiles.Count -eq 0) { throw 'Rust licenses directory contains no license texts.' }
foreach ($file in $rustFiles) {
    $relative = Get-RelativeSourceName $rustDirectory.FullName $file.FullName
    $sources.Add((Read-Notice $file.FullName "rust/$relative"))
}
foreach ($name in @('INNO-SETUP-LICENSE.txt', 'README.md')) {
    $sources.Add((Read-Notice (Join-Path $repositoryRoot "installer/languages/$name") "installer/languages/$name"))
}

# Preserve the Rust document itself instead of flattening its structured
# copyright notices, attribution, and license texts into an incomplete summary.
$bodyOpen = [regex]::Matches($rustCopyright.Text, '(?i)<body\b[^>]*>')
$bodyClose = [regex]::Matches($rustCopyright.Text, '(?i)</body\s*>')
if ($bodyOpen.Count -ne 1 -or $bodyClose.Count -ne 1 -or $bodyOpen[0].Index -ge $bodyClose[0].Index) {
    throw 'Rust copyright notices must contain exactly one complete HTML body.'
}
$intro = @'

<!-- color-picker-notices-introduction -->
<section id="color-picker-notices">
<style>
#color-picker-notices, #color-picker-dependency-notices { font-family: system-ui, sans-serif; line-height: 1.5; }
#color-picker-dependency-notices table { border-collapse: collapse; }
#color-picker-dependency-notices th, #color-picker-dependency-notices td { padding: .3rem .7rem; border: 1px solid #aaa; text-align: left; vertical-align: top; }
#color-picker-dependency-notices pre { white-space: pre-wrap; overflow-wrap: anywhere; padding: 1rem; background: #f4f4f4; color: #111; }
#color-picker-dependency-notices code { overflow-wrap: anywhere; }
</style>
<h1>Color Picker: third-party notices</h1>
<p>This self-contained document retains Rust standard-library notices below, followed by all locked Cargo runtime and build dependency license texts, Rust license files, and Inno Setup translation attribution. Some listed dependencies are used only during the build. A listed license is not a license for Color Picker itself; its license is in the separate LICENSE file.</p>
<p><a href="#color-picker-dependency-notices">Dependency inventory and complete license texts</a></p>
<p>Identical source files share one text only when their complete original bytes have the same SHA-256. Every source has its own logical path and checksum. Different copyright notices and alternative licenses are retained.</p>
</section>
<!-- /color-picker-notices-introduction -->

'@
$appendix = [Text.StringBuilder]::new()
$null = $appendix.AppendLine('<!-- color-picker-notices-appendix -->')
$null = $appendix.AppendLine('<section id="color-picker-dependency-notices"><h1>Dependency inventory and complete license texts</h1>')
$null = $appendix.AppendLine('<h2>Rust standard-library document</h2>')
$null = $appendix.AppendLine(('<p>The original HTML above is retained in full. Source: <code data-source="{0}" data-sha256="{1}">{0}</code>; SHA-256: <code>{1}</code>.</p>' -f
    (ConvertTo-HtmlText $rustCopyright.Name), $rustCopyright.Hash))
$null = $appendix.AppendLine('<h2>Cargo dependencies</h2><table><thead><tr><th>Package</th><th>Version</th><th>Declared license</th><th>Original files</th></tr></thead><tbody>')
foreach ($package in $packages) {
    $links = @($package.Sources | ForEach-Object {
        '<a href="#color-picker-license-{0}">{1}</a>' -f $_.Hash, (ConvertTo-HtmlText $_.Name)
    }) -join '<br>'
    $null = $appendix.AppendLine(('<tr><td>{0}</td><td>{1}</td><td>{2}</td><td>{3}</td></tr>' -f
        (ConvertTo-HtmlText $package.Name), (ConvertTo-HtmlText $package.Version),
        (ConvertTo-HtmlText $package.License), $links))
}
$null = $appendix.AppendLine('</tbody></table><h2>Original source files</h2>')
# Group by hash after collecting each logical source. No per-license-name or
# SPDX-expression deduplication: differing copyright text must remain intact.
foreach ($group in ($sources | Group-Object Hash | Sort-Object Name)) {
    $first = $group.Group[0]
    $null = $appendix.AppendLine(('<section id="color-picker-license-{0}"><h3>{1}</h3><ul>' -f
        $first.Hash, (ConvertTo-HtmlText $first.Name)))
    foreach ($source in ($group.Group | Sort-Object Name)) {
        $null = $appendix.AppendLine(('<li data-source="{0}" data-sha256="{1}"><code>{0}</code></li>' -f
            (ConvertTo-HtmlText $source.Name), $source.Hash))
    }
    $null = $appendix.AppendLine(('<p>SHA-256: <code>{0}</code></p>' -f $first.Hash))
    $null = $appendix.AppendLine(('<pre data-sha256="{0}">{1}</pre></section>' -f $first.Hash, (ConvertTo-HtmlText $first.Text)))
}
$null = $appendix.AppendLine('</section>')
$null = $appendix.AppendLine('<!-- /color-picker-notices-appendix -->')
$html = $rustCopyright.Text.Insert($bodyClose[0].Index, $appendix.ToString())
$html = $html.Insert($bodyOpen[0].Index + $bodyOpen[0].Length, $intro)
$outputBytes = $utf8.GetBytes($html)
# CreateNew also closes the race between the initial validation and final write.
$stream = [IO.FileStream]::new($output, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
try { $stream.Write($outputBytes, 0, $outputBytes.Length) }
finally { $stream.Dispose() }
Write-Host ('Third-party notices: {0} packages, {1} original sources, {2} distinct plain-text notices.' -f
    $packages.Count, ($sources.Count + 1), @($sources | Group-Object Hash).Count)
