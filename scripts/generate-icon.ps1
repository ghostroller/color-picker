[CmdletBinding()]
param()

# Regenerate the checked-in ICO from our original geometric SVG. Normal Rust
# builds use the ICO directly and do not need this script or an SVG renderer.
# This intentionally supports only the SVG primitives used by app.svg.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
[xml] $svg = Get-Content -LiteralPath (Join-Path $repositoryRoot 'resources/app.svg') -Raw
$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$frames = [Collections.Generic.List[byte[]]]::new()

function Get-Number($Element, [string] $Name) {
    return [float]::Parse($Element.GetAttribute($Name), [Globalization.CultureInfo]::InvariantCulture)
}

foreach ($size in $sizes) {
    $large = [Drawing.Bitmap]::new($size * 4, $size * 4, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [Drawing.Graphics]::FromImage($large)
    try {
        $graphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.Clear([Drawing.Color]::Transparent)
        $graphics.ScaleTransform($size / 16.0, $size / 16.0)
        foreach ($element in $svg.DocumentElement.ChildNodes) {
            if ($element.LocalName -in @('title', 'desc')) { continue }
            $path = [Drawing.Drawing2D.GraphicsPath]::new()
            try {
                switch ($element.LocalName) {
                    'rect' {
                        $x = Get-Number $element 'x'; $y = Get-Number $element 'y'
                        $width = Get-Number $element 'width'; $height = Get-Number $element 'height'
                        $diameter = 2 * (Get-Number $element 'rx')
                        $path.AddArc($x, $y, $diameter, $diameter, 180, 90)
                        $path.AddArc($x + $width - $diameter, $y, $diameter, $diameter, 270, 90)
                        $path.AddArc($x + $width - $diameter, $y + $height - $diameter, $diameter, $diameter, 0, 90)
                        $path.AddArc($x, $y + $height - $diameter, $diameter, $diameter, 90, 90)
                        $path.CloseFigure()
                    }
                    'line' {
                        $path.AddLine((Get-Number $element 'x1'), (Get-Number $element 'y1'),
                            (Get-Number $element 'x2'), (Get-Number $element 'y2'))
                    }
                    'circle' {
                        $radius = Get-Number $element 'r'
                        $path.AddEllipse((Get-Number $element 'cx') - $radius,
                            (Get-Number $element 'cy') - $radius, $radius * 2, $radius * 2)
                    }
                    'polygon' {
                        $coordinates = $element.GetAttribute('points').Trim() -split '[,\s]+'
                        $points = [Collections.Generic.List[Drawing.PointF]]::new()
                        for ($point = 0; $point -lt $coordinates.Count; $point += 2) {
                            $x = [float]::Parse($coordinates[$point], [Globalization.CultureInfo]::InvariantCulture)
                            $y = [float]::Parse($coordinates[$point + 1], [Globalization.CultureInfo]::InvariantCulture)
                            $points.Add([Drawing.PointF]::new($x, $y))
                        }
                        $path.AddPolygon($points.ToArray())
                    }
                    default { throw "Unsupported icon primitive: $($element.LocalName)" }
                }
                if ($element.HasAttribute('fill') -and $element.GetAttribute('fill') -ne 'none') {
                    $brush = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml($element.GetAttribute('fill')))
                    try { $graphics.FillPath($brush, $path) } finally { $brush.Dispose() }
                }
                if ($element.HasAttribute('stroke')) {
                    $pen = [Drawing.Pen]::new([Drawing.ColorTranslator]::FromHtml($element.GetAttribute('stroke')),
                        (Get-Number $element 'stroke-width'))
                    try {
                        if ($element.GetAttribute('stroke-linecap') -eq 'round') {
                            $pen.StartCap = [Drawing.Drawing2D.LineCap]::Round
                            $pen.EndCap = [Drawing.Drawing2D.LineCap]::Round
                        }
                        if ($element.GetAttribute('stroke-linejoin') -eq 'round') {
                            $pen.LineJoin = [Drawing.Drawing2D.LineJoin]::Round
                        }
                        $graphics.DrawPath($pen, $path)
                    } finally { $pen.Dispose() }
                }
            } finally { $path.Dispose() }
        }
        $frame = [Drawing.Bitmap]::new($size, $size, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $scaled = [Drawing.Graphics]::FromImage($frame)
        $stream = [IO.MemoryStream]::new()
        try {
            $scaled.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $scaled.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $scaled.DrawImage($large, [Drawing.Rectangle]::new(0, 0, $size, $size))
            $frame.Save($stream, [Drawing.Imaging.ImageFormat]::Png)
            $frames.Add($stream.ToArray())
        } finally {
            $stream.Dispose(); $scaled.Dispose(); $frame.Dispose()
        }
    } finally { $graphics.Dispose(); $large.Dispose() }
}

$output = Join-Path $repositoryRoot 'resources/app.ico'
$file = [IO.File]::Create($output)
$writer = [IO.BinaryWriter]::new($file)
try {
    $writer.Write([uint16] 0); $writer.Write([uint16] 1); $writer.Write([uint16] $sizes.Count)
    $offset = 6 + 16 * $sizes.Count
    for ($index = 0; $index -lt $sizes.Count; $index++) {
        $dimension = if ($sizes[$index] -eq 256) { 0 } else { $sizes[$index] }
        $writer.Write([byte] $dimension); $writer.Write([byte] $dimension)
        $writer.Write([byte] 0); $writer.Write([byte] 0)
        $writer.Write([uint16] 1); $writer.Write([uint16] 32)
        $writer.Write([uint32] $frames[$index].Length); $writer.Write([uint32] $offset)
        $offset += $frames[$index].Length
    }
    foreach ($frame in $frames) { $writer.Write($frame) }
} finally { $writer.Dispose(); $file.Dispose() }
Write-Host "Generated resources/app.ico ($($sizes -join ', ') px) from resources/app.svg"
