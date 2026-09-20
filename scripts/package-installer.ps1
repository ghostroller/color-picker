[CmdletBinding()]
param(
    # Use only after verification passed for these exact sources/toolchain.
    [switch] $SkipChecks,
    [switch] $Release,
    [string] $OutputManifestPath,
    [string] $CompilerPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Installer packaging requires Windows x64.' }
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ($OutputManifestPath) { $OutputManifestPath = [IO.Path]::GetFullPath($OutputManifestPath) }
Push-Location -LiteralPath $repositoryRoot
try {
    $metadataJson = & cargo metadata --no-deps --locked --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Could not read package version.' }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $version = ($metadata.packages | Where-Object { $_.name -eq 'color-picker' }).version
    # A single stable numeric ordering for EXE resources, installed versions and downgrade checks.
    # Pre-release SemVer needs an explicit channel/ordering design before it can be installed.
    if ($version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$' -or
        @($version.Split('.') | Where-Object { [long]$_ -gt 65535 }).Count -ne 0) {
        throw 'Installer versions must be major.minor.patch with each component between 0 and 65535.'
    }
    $compiler = & (Join-Path $PSScriptRoot 'find-inno-setup.ps1') -CompilerPath $CompilerPath
    $scratch = Join-Path $repositoryRoot 'target/installer-manifests'
    New-Item -ItemType Directory -Path $scratch -Force | Out-Null
    $portableManifest = Join-Path $scratch "$([Guid]::NewGuid().ToString('N')).json"
    & (Join-Path $PSScriptRoot 'package-windows.ps1') -SkipChecks:$SkipChecks -Release:$Release -OutputManifestPath $portableManifest
    $package = Get-Content -LiteralPath $portableManifest -Raw | ConvertFrom-Json
    $exe = Join-Path $package.package_directory 'color-picker.exe'
    if ((Get-Item -LiteralPath $exe).VersionInfo.ProductVersion -cne $version) {
        throw 'Executable version does not match Cargo.toml; refusing to package inconsistent binaries.'
    }
    $dist = Split-Path -Parent $package.package_directory
    $baseName = "$(Split-Path -Leaf $package.package_directory)-setup"
    $installer = Join-Path $dist "$baseName.exe"
    if (Test-Path -LiteralPath $installer) { throw "Output already exists: $installer" }
    & $compiler '/Qp' "/DAppVersion=$version" "/DVersionInfoVersion=$version.0" `
        "/DPayloadDir=$($package.package_directory)" "/DOutputDir=$dist" "/DOutputBaseName=$baseName" `
        (Join-Path $repositoryRoot 'installer/color-picker.iss') | Out-Host
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $installer)) { throw 'Inno Setup compilation failed.' }
    $hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$installer.sha256" -Value "$hash  $baseName.exe" -Encoding ASCII
    $pin = Get-Content -LiteralPath (Join-Path $repositoryRoot 'installer/toolchain.json') -Raw | ConvertFrom-Json
    $manifest = [ordered]@{
        version = $package.version
        source_commit = $package.source_commit
        source_dirty = $package.source_dirty
        installer_path = $installer
        installer_sha256_path = "$installer.sha256"
        archive_path = $package.archive_path
        archive_sha256_path = $package.archive_sha256_path
        package_directory = $package.package_directory
        inno_setup_version = $pin.version
        inno_setup_pinned_download_sha256 = $pin.sha256
        inno_setup_iscc_sha256 = (Get-FileHash -LiteralPath $compiler -Algorithm SHA256).Hash.ToLowerInvariant()
        signed = $false
    }
    $manifest | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $dist "$baseName.build.json") -Encoding UTF8
    if ($OutputManifestPath) {
        $manifest | ConvertTo-Json | Set-Content -LiteralPath $OutputManifestPath -Encoding UTF8
    }
    Write-Host "Installer: $installer"
    Write-Host "SHA256:   $hash"
}
finally { Pop-Location }
