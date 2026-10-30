# Regenerates assets/tasca.ico from the icon sources.
#
#   pwsh -File scripts/make-icon.ps1          # render + pack + verify
#   pwsh -File scripts/make-icon.ps1 -Verify  # only check the committed .ico
#
# Needs nothing but Windows: a Chromium browser (Chrome or Edge, both already on
# Windows 10/11) rasterises the SVGs, .NET packs and checks them. No npm, no pip,
# no Cargo dependencies. assets/tasca.svg is the drawing; assets/tasca-small.svg
# is the hand-tuned variant used at 16 and 20 px, where the full one is mush.
param([switch]$Verify)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Sizes = 16, 20, 24, 32, 48, 256
$SmallSizes = 16, 20   # rendered from tasca-small.svg
$Root = Split-Path -Parent $PSScriptRoot
$Ico = Join-Path $Root 'assets\tasca.ico'
$Svg = Join-Path $Root 'assets\tasca.svg'
$SvgSmall = Join-Path $Root 'assets\tasca-small.svg'

Add-Type -AssemblyName System.Drawing

function Find-Browser {
    $candidates = @(
        "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
        "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
        "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
        "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
        "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe"
    )
    foreach ($c in $candidates) { if (Test-Path -LiteralPath $c) { return $c } }
    throw 'No Chrome or Edge found; one of them is needed to rasterise the SVGs.'
}

function Render-Svg([string]$Browser, [string]$SvgPath, [string]$OutPng, [int]$Size) {
    # Render in a fixed 512x512 window and crop the top-left Size x Size: Chrome
    # clamps a window smaller than its minimum width, which shifts the drawing.
    $work = Join-Path $env:TEMP ("tasca-icon-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    try {
        $svg = Get-Content -LiteralPath $SvgPath -Raw
        $html = @"
<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;padding:0;background:transparent;overflow:hidden}
svg{display:block;width:${Size}px;height:${Size}px}
</style></head><body>$svg</body></html>
"@
        $page = Join-Path $work 'icon.html'
        Set-Content -LiteralPath $page -Value $html -Encoding UTF8
        $url = 'file:///' + ($page -replace '\\', '/')
        $shot = Join-Path $work 'shot.png'
        $cliArgs = @(
            '--headless=new', '--disable-gpu', '--hide-scrollbars', '--no-first-run',
            '--no-default-browser-check', '--force-device-scale-factor=1',
            '--default-background-color=00000000',
            '--window-size=512,512',
            "--user-data-dir=$(Join-Path $work 'profile')",
            "--screenshot=$shot",
            $url
        )
        $proc = Start-Process -FilePath $Browser -ArgumentList $cliArgs -PassThru -Wait
        # Some builds detach before the file lands; give it a moment.
        $deadline = (Get-Date).AddSeconds(10)
        while (-not (Test-Path -LiteralPath $shot) -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 100
        }
        if (-not (Test-Path -LiteralPath $shot)) {
            throw "Renderer exited $($proc.ExitCode) without writing $shot"
        }
        $bmp = [System.Drawing.Bitmap]::FromFile($shot)
        try {
            if ($bmp.Width -lt $Size -or $bmp.Height -lt $Size) {
                throw "Rendered $($bmp.Width)x$($bmp.Height), wanted at least ${Size}x$Size"
            }
            $crop = New-Object System.Drawing.Bitmap($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
            try {
                $g = [System.Drawing.Graphics]::FromImage($crop)
                try {
                    $g.DrawImage($bmp, (New-Object System.Drawing.Rectangle(0, 0, $Size, $Size)),
                        (New-Object System.Drawing.Rectangle(0, 0, $Size, $Size)),
                        [System.Drawing.GraphicsUnit]::Pixel)
                }
                finally { $g.Dispose() }
                $crop.Save($OutPng, [System.Drawing.Imaging.ImageFormat]::Png)
            }
            finally { $crop.Dispose() }
        }
        finally { $bmp.Dispose() }
    }
    finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Get-IcoEntries([string]$Path) {
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 6) { throw "$Path is too short to be an icon" }
    $count = [int]$bytes[4] -bor ([int]$bytes[5] -shl 8)
    if ($bytes[2] -ne 1 -or $bytes[3] -ne 0) { throw "$Path is not an icon (type $($bytes[2]))" }
    $entries = @()
    for ($i = 0; $i -lt $count; $i++) {
        $o = 6 + 16 * $i
        $w = if ($bytes[$o] -eq 0) { 256 } else { [int]$bytes[$o] }
        $h = if ($bytes[$o + 1] -eq 0) { 256 } else { [int]$bytes[$o + 1] }
        $len = [BitConverter]::ToInt32($bytes, $o + 8)
        $off = [BitConverter]::ToInt32($bytes, $o + 12)
        $png = $bytes[$off] -eq 0x89 -and $bytes[$off + 1] -eq 0x50 -and
               $bytes[$off + 2] -eq 0x4E -and $bytes[$off + 3] -eq 0x47
        $entries += [pscustomobject]@{ Width = $w; Height = $h; Bytes = $len; Offset = $off; Png = $png }
    }
    return $entries
}

function Read-IconBitmap([string]$Path, [int]$Size, $Entry) {
    # Every entry is PNG; System.Drawing.Icon mis-decodes PNG entries, so slice
    # the PNG out and decode that.
    if (-not $Entry.Png) { throw "the $Size px entry of $Path is not PNG" }
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $slice = New-Object byte[] $Entry.Bytes
    [Array]::Copy($bytes, $Entry.Offset, $slice, 0, $Entry.Bytes)
    $ms = New-Object System.IO.MemoryStream(, $slice)
    return [System.Drawing.Bitmap]::FromStream($ms)
}

function Test-Icon([string]$Path) {
    $entries = Get-IcoEntries $Path
    $sizes = $entries | ForEach-Object { $_.Width }
    if (($sizes -join ',') -ne ($Sizes -join ',')) {
        throw "icon sizes are $($sizes -join ', '); expected $($Sizes -join ', ')"
    }
    foreach ($e in $entries) {
        if ($e.Width -ne $e.Height) { throw "entry $($e.Width)x$($e.Height) is not square" }
    }
    if (-not ($entries | Where-Object Width -eq 256).Png) { throw 'the 256 px entry must be PNG' }

    # Colour sanity: the tile is ivory with a rust panel.
    $big = Read-IconBitmap $Path 48 ($entries | Where-Object Width -eq 48)
    $rust = 0; $ivory = 0
    try {
        for ($y = 0; $y -lt $big.Height; $y++) {
            for ($x = 0; $x -lt $big.Width; $x++) {
                $p = $big.GetPixel($x, $y)
                if ($p.A -lt 128) { continue }
                if ($p.R -ge 225 -and $p.G -ge 215 -and $p.B -ge 190 -and $p.B -le 245 -and ($p.R - $p.B) -ge 5) { $ivory++ }
                if ($p.R -ge 120 -and ($p.R - $p.B) -ge 40 -and ($p.R - $p.G) -ge 20 -and $p.R -le 210) { $rust++ }
            }
        }
        if ($ivory -lt 200) { throw "48 px has $ivory ivory pixels; the tile is missing" }
        if ($rust -lt 40) { throw "48 px has $rust rust pixels; the rust panel is missing" }
    }
    finally { $big.Dispose() }

    $tiny = Read-IconBitmap $Path 16 ($entries | Where-Object Width -eq 16)
    $dark = 0; $warm = 0
    try {
        for ($y = 0; $y -lt 16; $y++) {
            for ($x = 0; $x -lt 16; $x++) {
                $p = $tiny.GetPixel($x, $y)
                if ($p.A -lt 128) { continue }
                $luma = 0.299 * $p.R + 0.587 * $p.G + 0.114 * $p.B
                if ($luma -lt 100) { $dark++ }
                if ($p.R -ge 120 -and ($p.R - $p.B) -ge 35) { $warm++ }
            }
        }
        # The Shelf at 16 px: a dark strip plus a rust panel.
        if ($dark -lt 20) { throw "16 px has $dark dark pixels; the dark strip is unreadable" }
        if ($warm -lt 4) { throw "16 px has $warm rust pixels; the rust panel is missing" }
    }
    finally { $tiny.Dispose() }

    "assets/tasca.ico: $($entries.Count) entries, sizes $($sizes -join ', '); ivory $ivory, rust $rust, 16 px dark $dark / warm $warm"
}

if (-not $Verify) {
    $browser = Find-Browser
    $tmp = Join-Path $env:TEMP ("tasca-icon-build-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        $pngs = @{}
        foreach ($size in $Sizes) {
            $source = if ($SmallSizes -contains $size) { $SvgSmall } else { $Svg }
            $out = Join-Path $tmp "icon-$size.png"
            Render-Svg -Browser $browser -SvgPath $source -OutPng $out -Size $size
            $pngs[$size] = [System.IO.File]::ReadAllBytes($out)
        }

        $stream = [System.IO.MemoryStream]::new()
        $writer = [System.IO.BinaryWriter]::new($stream)
        try {
            $writer.Write([uint16]0)                # reserved
            $writer.Write([uint16]1)                # 1 = icon
            $writer.Write([uint16]$pngs.Count)
            $offset = 6 + 16 * $pngs.Count
            foreach ($size in $Sizes) {
                $writer.Write([byte]($(if ($size -eq 256) { 0 } else { $size })))  # width
                $writer.Write([byte]($(if ($size -eq 256) { 0 } else { $size })))  # height
                $writer.Write([byte]0)              # palette colours
                $writer.Write([byte]0)              # reserved
                $writer.Write([uint16]1)            # planes
                $writer.Write([uint16]32)           # bits per pixel
                $writer.Write([uint32]$pngs[$size].Length)
                $writer.Write([uint32]$offset)
                $offset += $pngs[$size].Length
            }
            foreach ($size in $Sizes) { $writer.Write($pngs[$size]) }
            $writer.Flush()
            [System.IO.File]::WriteAllBytes($Ico, $stream.ToArray())
        }
        finally { $writer.Dispose(); $stream.Dispose() }
    }
    finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Test-Icon $Ico
