[CmdletBinding()]
param(
    [string]$OutputPath = (Join-Path $PSScriptRoot '..\assets\VanGoal.ico'),
    [switch]$UpdateAppIcon
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

function Add-U16([System.Collections.Generic.List[byte]]$Bytes, [int]$Value) {
    foreach ($byte in [BitConverter]::GetBytes([uint16]$Value)) {
        $Bytes.Add($byte)
    }
}

function Add-U32([System.Collections.Generic.List[byte]]$Bytes, [int64]$Value) {
    foreach ($byte in [BitConverter]::GetBytes([uint32]$Value)) {
        $Bytes.Add($byte)
    }
}

function Test-RoundedIconPixel([int]$Size, [int]$X, [int]$Y) {
    $radius = [int][Math]::Round($Size * 0.18)
    if ($radius -le 0) {
        return $true
    }

    if ($X -ge $radius -and $X -lt ($Size - $radius)) {
        return $true
    }
    if ($Y -ge $radius -and $Y -lt ($Size - $radius)) {
        return $true
    }

    $centerX = if ($X -lt $radius) { $radius - 1 } else { $Size - $radius }
    $centerY = if ($Y -lt $radius) { $radius - 1 } else { $Size - $radius }
    $dx = $X - $centerX
    $dy = $Y - $centerY
    return (($dx * $dx) + ($dy * $dy)) -le ($radius * $radius)
}

function Write-RoundedAppIcon([string]$RootPath) {
    $sourcePath = Join-Path $RootPath 'assets/AppIcon.png'
    $temporaryPath = Join-Path $RootPath 'assets/AppIcon.rounded.png'
    $source = [System.Drawing.Bitmap]::new($sourcePath)
    $output = [System.Drawing.Bitmap]::new(
        $source.Width,
        $source.Height,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    try {
        for ($y = 0; $y -lt $source.Height; $y++) {
            for ($x = 0; $x -lt $source.Width; $x++) {
                $pixel = $source.GetPixel($x, $y)
                if (Test-RoundedIconPixel $source.Width $x $y) {
                    $output.SetPixel($x, $y, [System.Drawing.Color]::FromArgb(255, $pixel.R, $pixel.G, $pixel.B))
                } else {
                    $output.SetPixel($x, $y, [System.Drawing.Color]::Transparent)
                }
            }
        }
        $output.Save($temporaryPath, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $source.Dispose()
        $output.Dispose()
    }
    Move-Item -LiteralPath $temporaryPath -Destination $sourcePath -Force
    Write-Host "Rounded $sourcePath"
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ($UpdateAppIcon) {
    Write-RoundedAppIcon $root
}
$sizes = @(16, 32, 128, 256)
$frames = foreach ($size in $sizes) {
    $source = Join-Path $root "assets/Assets.xcassets/AppIcon.appiconset/icon_${size}x${size}.png"
    $bitmap = [System.Drawing.Bitmap]::new($source)
    try {
        if ($bitmap.Width -ne $size -or $bitmap.Height -ne $size) {
            throw "Unexpected source dimensions for $source"
        }

        # A classic ICO DIB is understood by LoadImageW, Explorer, and the
        # taskbar. Pixels are BGRA and stored bottom-up. The corners are
        # transparent so Windows does not show the source image's white square.
        $header = New-Object byte[] 40
        [Buffer]::BlockCopy([BitConverter]::GetBytes([uint32]40), 0, $header, 0, 4)
        [Buffer]::BlockCopy([BitConverter]::GetBytes([int32]$size), 0, $header, 4, 4)
        [Buffer]::BlockCopy([BitConverter]::GetBytes([int32]($size * 2)), 0, $header, 8, 4)
        [Buffer]::BlockCopy([BitConverter]::GetBytes([uint16]1), 0, $header, 12, 2)
        [Buffer]::BlockCopy([BitConverter]::GetBytes([uint16]32), 0, $header, 14, 2)
        [Buffer]::BlockCopy([BitConverter]::GetBytes([uint32]($size * $size * 4)), 0, $header, 20, 4)

        $xor = New-Object byte[] ($size * $size * 4)
        $maskRowBytes = [int]([Math]::Ceiling($size / 32.0) * 4)
        $mask = New-Object byte[] ($maskRowBytes * $size)
        $offset = 0
        for ($y = $size - 1; $y -ge 0; $y--) {
            for ($x = 0; $x -lt $size; $x++) {
                $pixel = $bitmap.GetPixel($x, $y)
                $inside = Test-RoundedIconPixel $size $x $y
                if ($inside) {
                    $xor[$offset++] = $pixel.B
                    $xor[$offset++] = $pixel.G
                    $xor[$offset++] = $pixel.R
                    $xor[$offset++] = $pixel.A
                } else {
                    $xor[$offset++] = 0
                    $xor[$offset++] = 0
                    $xor[$offset++] = 0
                    $xor[$offset++] = 0
                }

                if (-not $inside) {
                    $maskRow = $size - 1 - $y
                    $maskIndex = ($maskRow * $maskRowBytes) + [int][Math]::Floor($x / 8.0)
                    $mask[$maskIndex] = $mask[$maskIndex] -bor (0x80 -shr ($x % 8))
                }
            }
        }

        $data = New-Object byte[] ($header.Length + $xor.Length + $mask.Length)
        [Buffer]::BlockCopy($header, 0, $data, 0, $header.Length)
        [Buffer]::BlockCopy($xor, 0, $data, $header.Length, $xor.Length)
        [Buffer]::BlockCopy($mask, 0, $data, $header.Length + $xor.Length, $mask.Length)
        [pscustomobject]@{ Size = $size; Data = $data }
    }
    finally {
        $bitmap.Dispose()
    }
}

$ico = [System.Collections.Generic.List[byte]]::new()
Add-U16 $ico 0
Add-U16 $ico 1
Add-U16 $ico $frames.Count
$offset = 6 + (16 * $frames.Count)
foreach ($frame in $frames) {
    $dimension = if ($frame.Size -ge 256) { 0 } else { $frame.Size }
    $maskRowBytes = [int]([Math]::Ceiling($frame.Size / 32.0) * 4)
    $dataLength = 40 + ($frame.Size * $frame.Size * 4) + ($maskRowBytes * $frame.Size)
    $ico.Add([byte]$dimension)
    $ico.Add([byte]$dimension)
    $ico.Add([byte]0)
    $ico.Add([byte]0)
    Add-U16 $ico 1
    Add-U16 $ico 32
    Add-U32 $ico $dataLength
    Add-U32 $ico $offset
    $offset += $dataLength
}
foreach ($frame in $frames) {
    $ico.AddRange($frame.Data)
}

$destination = if ([System.IO.Path]::IsPathRooted($OutputPath)) {
    [System.IO.Path]::GetFullPath($OutputPath)
} else {
    [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $OutputPath))
}
[System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($destination)) | Out-Null
[System.IO.File]::WriteAllBytes($destination, $ico.ToArray())
Write-Host "Generated $destination ($($ico.Count) bytes)"
