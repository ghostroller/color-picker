[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') {
    throw 'This verification script requires Windows and the MSVC toolchain.'
}

function Invoke-CheckedNative {
    param(
        [Parameter(Mandatory)] [string] $FilePath,
        [Parameter(Mandatory)] [string[]] $Arguments
    )

    Write-Host "> $FilePath $($Arguments -join ' ')"
    # A pipeline also waits for GUI-subsystem executables before checking their exit code.
    & $FilePath @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath failed with exit code $LASTEXITCODE."
    }
}

function Find-ManifestTool {
    # SDK tools are not normally on PATH. Use the developer environment or installed SDK registry.
    if ($env:WindowsSdkVerBinPath) {
        $candidate = Join-Path $env:WindowsSdkVerBinPath 'x64\mt.exe'
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }

    $sdkRoots = @()
    if ($env:WindowsSdkDir) {
        $sdkRoots += $env:WindowsSdkDir
    }
    foreach ($registryPath in @(
        'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots'
    )) {
        $installedRoot = Get-ItemPropertyValue -LiteralPath $registryPath -Name KitsRoot10 -ErrorAction SilentlyContinue
        if ($installedRoot) {
            $sdkRoots += $installedRoot
        }
    }

    foreach ($sdkRoot in ($sdkRoots | Select-Object -Unique)) {
        $binRoot = Join-Path $sdkRoot 'bin'
        if (-not (Test-Path -LiteralPath $binRoot -PathType Container)) {
            continue
        }
        $versions = Get-ChildItem -LiteralPath $binRoot -Directory |
            Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
            Sort-Object { [version] $_.Name } -Descending
        foreach ($version in $versions) {
            $candidate = Join-Path $version.FullName 'x64\mt.exe'
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                return $candidate
            }
        }
    }

    throw 'Windows SDK mt.exe was not found. Install the Windows SDK or run from its developer environment.'
}

$repositoryRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repositoryRoot
try {
    Invoke-CheckedNative cargo @('fmt', '--all', '--', '--check')
    Invoke-CheckedNative cargo @('clippy', '--all-targets', '--locked', '--', '-D', 'warnings')
    Invoke-CheckedNative cargo @('test', '--locked')
    Invoke-CheckedNative cargo @('build', '--release', '--locked', '--target', 'x86_64-pc-windows-msvc')

    # Cargo owns target directory resolution (including CARGO_TARGET_DIR and config overrides).
    $metadataJson = & cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE."
    }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $releaseDirectory = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc\release'
    $executable = Join-Path $releaseDirectory 'color-picker.exe'
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Release executable is missing: $executable"
    }

    $manifestTool = Find-ManifestTool
    $manifestPath = Join-Path $releaseDirectory 'color-picker.extracted.manifest'
    Write-Host "Windows SDK manifest tool: $manifestTool"
    # Extract the actual EXE resource RT_MANIFEST #1; reading app.manifest cannot prove embedding.
    Invoke-CheckedNative $manifestTool @('-nologo', "-inputresource:$executable;#1", "-out:$manifestPath")

    [xml] $manifest = Get-Content -LiteralPath $manifestPath -Raw
    $namespaces = [System.Xml.XmlNamespaceManager]::new($manifest.NameTable)
    $namespaces.AddNamespace('asm', 'urn:schemas-microsoft-com:asm.v1')
    $namespaces.AddNamespace('trust', 'urn:schemas-microsoft-com:asm.v3')
    $namespaces.AddNamespace('dpi', 'http://schemas.microsoft.com/SMI/2016/WindowsSettings')

    $dpi = $manifest.SelectSingleNode('//dpi:dpiAwareness', $namespaces)
    if ($null -eq $dpi -or $dpi.InnerText.Trim() -cne 'PerMonitorV2') {
        throw 'The EXE manifest must declare PerMonitorV2 DPI awareness.'
    }
    $executionLevel = $manifest.SelectSingleNode('//trust:requestedExecutionLevel', $namespaces)
    if ($null -eq $executionLevel -or $executionLevel.GetAttribute('level') -cne 'asInvoker' -or
        $executionLevel.GetAttribute('uiAccess') -cne 'false') {
        throw 'The EXE manifest must declare asInvoker with uiAccess=false.'
    }
    $commonControls = $manifest.SelectSingleNode(
        '/asm:assembly/asm:dependency/asm:dependentAssembly/asm:assemblyIdentity[@name="Microsoft.Windows.Common-Controls"]',
        $namespaces
    )
    if ($null -eq $commonControls -or $commonControls.GetAttribute('version') -cne '6.0.0.0' -or
        $commonControls.GetAttribute('publicKeyToken') -cne '6595b64144ccf1df') {
        throw 'The EXE manifest must depend on Microsoft.Windows.Common-Controls 6.0.0.0.'
    }

    # This mode checks the effective process DPI context and exits; it installs no input hooks.
    Invoke-CheckedNative $executable @('--check-environment')
    Write-Host 'Windows build, embedded manifest, and process DPI checks passed.'
}
finally {
    Pop-Location
}
