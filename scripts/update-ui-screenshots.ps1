<#
.SYNOPSIS
Rebuild the documentation screenshots from the production Windows UI fixture.
.DESCRIPTION
Run on an interactive Windows desktop with the MSVC toolchain. The fixture uses
default settings and synthetic sample pixels; it does not save user settings.
PNG files retain the exported client area's native pixels without resizing.
All captures and conversions must succeed before docs/images is updated.
.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/update-ui-screenshots.ps1
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') {
    throw 'Screenshot generation requires Windows and the MSVC toolchain.'
}
if (-not [Environment]::UserInteractive) {
    throw 'Run screenshot generation from an interactive Windows desktop.'
}

Add-Type -AssemblyName System.Drawing

function Invoke-CheckedNative {
    param(
        [Parameter(Mandatory)] [string] $FilePath,
        [Parameter(Mandatory)] [string[]] $Arguments
    )

    Write-Host "> $FilePath $($Arguments -join ' ')"
    # The pipeline waits for GUI-subsystem executables before checking exit status.
    & $FilePath @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath failed with exit code $LASTEXITCODE."
    }
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$target = 'x86_64-pc-windows-msvc'
$runDirectory = $null
$temporaryFiles = [Collections.Generic.List[string]]::new()
$captures = @(
    @{ Name = 'ui-result'; Mode = 'result'; Language = 'zh-CN'; Extra = @() }
    @{ Name = 'ui-result-en'; Mode = 'result'; Language = 'en'; Extra = @() }
    @{ Name = 'ui-settings'; Mode = 'settings'; Language = 'zh-CN'; Extra = @() }
    @{ Name = 'ui-settings-en'; Mode = 'settings'; Language = 'en'; Extra = @() }
    @{ Name = 'ui-settings-compact'; Mode = 'settings'; Language = 'zh-CN'; Extra = @('--settings-size=500x320') }
    @{ Name = 'ui-settings-compact-en'; Mode = 'settings'; Language = 'en'; Extra = @('--settings-size=500x320') }
    @{ Name = 'ui-live'; Mode = 'live'; Language = 'zh-CN'; Extra = @('--backdrop') }
    @{ Name = 'ui-live-en'; Mode = 'live'; Language = 'en'; Extra = @('--backdrop') }
    @{ Name = 'ui-frozen'; Mode = 'frozen'; Language = 'zh-CN'; Extra = @('--backdrop') }
    @{ Name = 'ui-frozen-en'; Mode = 'frozen'; Language = 'en'; Extra = @('--backdrop') }
    @{ Name = 'ui-frozen-edge'; Mode = 'frozen-edge'; Language = 'zh-CN'; Extra = @('--backdrop') }
)

Push-Location -LiteralPath $repositoryRoot
try {
    Invoke-CheckedNative cargo @('build', '--example', 'ui-preview', '--release', '--locked', '--target', $target)

    # Let Cargo resolve CARGO_TARGET_DIR and repository/user Cargo configuration.
    $metadataJson = & cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE."
    }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $project = $metadata.packages | Where-Object { $_.name -eq 'color-picker' }
    $executable = Join-Path $metadata.target_directory "$target/release/examples/ui-preview.exe"
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "UI fixture executable is missing: $executable"
    }

    $temporaryRoot = Join-Path $metadata.target_directory 'ui-screenshots'
    $runDirectory = Join-Path $temporaryRoot ([Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $runDirectory | Out-Null
    $completed = [Collections.Generic.List[object]]::new()
    Write-Host "Color Picker $($project.version): Release, locked, $target"
    Write-Host "Temporary captures: $runDirectory"

    foreach ($capture in $captures) {
        $bitmapPath = Join-Path $runDirectory "$($capture.Name).bmp"
        $pngPath = Join-Path $runDirectory "$($capture.Name).png"
        $temporaryFiles.Add($bitmapPath)
        $temporaryFiles.Add($pngPath)
        $fixtureArguments = @($capture.Mode, "--language=$($capture.Language)") +
            $capture.Extra + @("--output=$bitmapPath")
        Invoke-CheckedNative $executable $fixtureArguments

        # Encode the exact native-size bitmap. No screen crop, scaling or redraw.
        $bitmap = [Drawing.Bitmap]::new($bitmapPath)
        try {
            $width = $bitmap.Width
            $height = $bitmap.Height
            $bitmap.Save($pngPath, [Drawing.Imaging.ImageFormat]::Png)
        }
        finally {
            $bitmap.Dispose()
        }

        # Reopen the completed PNG before allowing it to replace a document image.
        $png = [Drawing.Bitmap]::new($pngPath)
        try {
            if ($png.Width -ne $width -or $png.Height -ne $height) {
                throw "PNG dimensions differ from the exported bitmap: $($capture.Name)"
            }
        }
        finally {
            $png.Dispose()
        }
        $completed.Add([pscustomobject]@{
            File = "$($capture.Name).png"
            Language = $capture.Language
            Pixels = "${width}x${height}"
            Source = $pngPath
        })
    }

    $imageDirectory = Join-Path $repositoryRoot 'docs/images'
    foreach ($image in $completed) {
        Copy-Item -LiteralPath $image.Source -Destination (Join-Path $imageDirectory $image.File) -Force
    }
    $completed | Format-Table File, Language, Pixels -AutoSize | Out-Host
    Write-Host "Updated $($completed.Count) screenshots in $imageDirectory"
}
finally {
    # Only remove this invocation's named files; never recursively clean Cargo output.
    foreach ($path in $temporaryFiles) {
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            Remove-Item -LiteralPath $path -Force
        }
    }
    if ($runDirectory -and (Test-Path -LiteralPath $runDirectory -PathType Container)) {
        [IO.Directory]::Delete($runDirectory, $false)
    }
    Pop-Location
}
