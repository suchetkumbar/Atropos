#Requires -Version 7
<#
.SYNOPSIS
  Generates the Atropos icon: a glowing thread, cut, on a dark rounded square.
.DESCRIPTION
  Writes assets/atropos.ico (16, 24, 32, 48, 64, 128, 256 px; PNG-compressed entries) and
  assets/atropos-64.rgba (raw 64x64 RGBA, used as the window/taskbar icon at runtime).
  Each size is drawn directly, not downscaled, so it stays crisp at 16 px.
#>
param([string]$OutDir = (Join-Path $PSScriptRoot '..\assets'))
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force $OutDir | Out-Null

function New-IconBitmap([int]$s) {
    $bmp = [System.Drawing.Bitmap]::new($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'; $g.PixelOffsetMode = 'HighQuality'; $g.CompositingQuality = 'HighQuality'
    $g.Clear([System.Drawing.Color]::Transparent)
    function P([double]$x, [double]$y) { [System.Drawing.PointF]::new([float]($x * $s), [float]($y * $s)) }
    function W([double]$f, [double]$min) { [float][Math]::Max($f * $s, $min) }

    # Background: rounded square, deep violet to near-black.
    $r = [float]($s * 0.22); $inset = [float]([Math]::Max($s * 0.02, 0.5)); $w = [float]($s - 2 * $inset)
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $path.AddArc($inset, $inset, 2 * $r, 2 * $r, 180, 90)
    $path.AddArc($inset + $w - 2 * $r, $inset, 2 * $r, 2 * $r, 270, 90)
    $path.AddArc($inset + $w - 2 * $r, $inset + $w - 2 * $r, 2 * $r, 2 * $r, 0, 90)
    $path.AddArc($inset, $inset + $w - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $path.CloseFigure()
    $bg = [System.Drawing.Drawing2D.LinearGradientBrush]::new((P 0 0), (P 1 1),
        [System.Drawing.Color]::FromArgb(255, 46, 20, 92), [System.Drawing.Color]::FromArgb(255, 10, 8, 22))
    $g.FillPath($bg, $path)
    if ($s -ge 32) {
        $edge = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(110, 150, 110, 255), (W 0.012 1))
        $g.DrawPath($edge, $path)
    }

    # Thread: two halves of one S-curve with a gap where it was cut.
    $left  = @((P 0.12 0.72), (P 0.30 0.74), (P 0.33 0.55), (P 0.42 0.52))
    $right = @((P 0.58 0.46), (P 0.67 0.43), (P 0.70 0.26), (P 0.88 0.28))
    $glowA = [System.Drawing.Color]::FromArgb(38, 255, 110, 90)
    $glowB = [System.Drawing.Color]::FromArgb(70, 255, 170, 90)
    foreach ($glow in @(@($glowA, (W 0.14 3)), @($glowB, (W 0.095 2)))) {
        $pen = [System.Drawing.Pen]::new($glow[0], $glow[1]); $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
        $g.DrawBezier($pen, $left[0], $left[1], $left[2], $left[3])
        $g.DrawBezier($pen, $right[0], $right[1], $right[2], $right[3])
    }
    $core = [System.Drawing.Drawing2D.LinearGradientBrush]::new((P 0.1 0.75), (P 0.9 0.25),
        [System.Drawing.Color]::FromArgb(255, 255, 64, 129), [System.Drawing.Color]::FromArgb(255, 255, 214, 92))
    $pen = [System.Drawing.Pen]::new($core, (W 0.045 1.6)); $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
    $g.DrawBezier($pen, $left[0], $left[1], $left[2], $left[3])
    $g.DrawBezier($pen, $right[0], $right[1], $right[2], $right[3])

    # The cut: a four-point spark between the frayed ends, plus a faint blade stroke.
    $cx = 0.50; $cy = 0.49
    if ($s -ge 24) {
        $blade = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(150, 235, 230, 255), (W 0.014 1))
        $blade.StartCap = 'Round'; $blade.EndCap = 'Round'
        $g.DrawLine($blade, (P 0.62 0.18), (P 0.38 0.80))
    }
    $halo = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $hr = [float]($s * 0.13); $hc = P $cx $cy
    $halo.AddEllipse($hc.X - $hr, $hc.Y - $hr, 2 * $hr, 2 * $hr)
    $hb = [System.Drawing.Drawing2D.PathGradientBrush]::new($halo)
    $hb.CenterColor = [System.Drawing.Color]::FromArgb(200, 255, 245, 200)
    $hb.SurroundColors = @([System.Drawing.Color]::FromArgb(0, 255, 180, 80))
    $g.FillPath($hb, $halo)
    $a = 0.11; $b = 0.022
    $star = @((P $cx ($cy - $a)), (P ($cx + $b) ($cy - $b)), (P ($cx + $a) $cy), (P ($cx + $b) ($cy + $b)),
              (P $cx ($cy + $a)), (P ($cx - $b) ($cy + $b)), (P ($cx - $a) $cy), (P ($cx - $b) ($cy - $b)))
    $g.FillPolygon([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 255, 252, 235)), $star)
    $g.Dispose()
    return $bmp
}

$sizes = 16, 24, 32, 48, 64, 128, 256
$pngs = foreach ($s in $sizes) {
    $bmp = New-IconBitmap $s
    $ms = [System.IO.MemoryStream]::new(); $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    if ($s -eq 64) {
        # Raw RGBA for the runtime window icon (System.Drawing is BGRA in memory).
        $raw = [byte[]]::new(64 * 64 * 4); $i = 0
        for ($y = 0; $y -lt 64; $y++) { for ($x = 0; $x -lt 64; $x++) {
            $c = $bmp.GetPixel($x, $y); $raw[$i++] = $c.R; $raw[$i++] = $c.G; $raw[$i++] = $c.B; $raw[$i++] = $c.A } }
        [System.IO.File]::WriteAllBytes((Join-Path $OutDir 'atropos-64.rgba'), $raw)
    }
    if ($s -eq 256) { $bmp.Save((Join-Path $OutDir 'atropos-256.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $bmp.Dispose()
    , $ms.ToArray()
}

# ICO container: ICONDIR + ICONDIRENTRY[n] + PNG payloads.
$out = [System.IO.MemoryStream]::new(); $bw = [System.IO.BinaryWriter]::new($out)
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($k = 0; $k -lt $sizes.Count; $k++) {
    $s = $sizes[$k]; $dim = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32); $bw.Write([uint32]$pngs[$k].Length); $bw.Write([uint32]$offset)
    $offset += $pngs[$k].Length
}
foreach ($p in $pngs) { $bw.Write($p) }
$bw.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'atropos.ico'), $out.ToArray())
Write-Host "Wrote atropos.ico ($($out.Length) bytes), atropos-64.rgba, atropos-256.png to $OutDir"
