<#
.SYNOPSIS
  アイコンの元（assets/sqltrace.svg）から、exe に埋め込む assets/sqltrace.ico、GUI の窓に使う
  assets/sqltrace-64.rgba、README に使う assets/sqltrace-256.png を作る。

.DESCRIPTION
  SVG は Edge（なければ Chrome）のヘッドレスで、地を透明にして大きさごとに描き、1 枚に撮って切り分ける。
  .ico は 16、20、24、32、40、48、64、128、256 px。256 は PNG、それより小さいものは 32 ビットの BMP で入れる
  （小さい絵を PNG で入れると読めない道具がある）。.rgba は 64x64 の RGBA を左上から並べただけのもの
  （GUI が画像の読み込みの依存なしに窓のアイコンにする）。依存は足さない。SVG を変えたら作り直してコミットする。
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Add-Type -AssemblyName System.Drawing

$browser = @(
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { throw 'Edge か Chrome が見つかりません。SVG を描くのに使います。' }

$icoSizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$work = Join-Path ([IO.Path]::GetTempPath()) "sqltrace-icon-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
Copy-Item (Join-Path $root 'assets\sqltrace.svg') (Join-Path $work 'icon.svg')

# 大きさごとに絵を横に並べた頁を作り、1 枚で撮って切り分ける。
$x = 0
$places = @{}
$html = New-Object System.Text.StringBuilder
[void]$html.Append('<!DOCTYPE html><html><head><style>html,body{margin:0;background:transparent;overflow:hidden}img{position:absolute;top:0}</style></head><body>')
foreach ($size in $icoSizes) {
    [void]$html.Append("<img src='icon.svg' width='$size' height='$size' style='left:${x}px'>")
    $places[$size] = $x
    $x += $size + 8
}
[void]$html.Append('</body></html>')
$sheetHtml = Join-Path $work 'sheet.html'
[IO.File]::WriteAllText($sheetHtml, $html.ToString(), (New-Object Text.UTF8Encoding $false))
$sheetPng = Join-Path $work 'sheet.png'
# 拡大率を 1 に決め（画面の設定によらず、決まった大きさで描く）、地を透明にする。
# ブラウザーは標準エラーにログを出す。それを誤りとみなさないよう、Start-Process で待つ。
$arguments = @('--headless=new', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1',
    '--default-background-color=00000000', "--window-size=$x,256", "--screenshot=`"$sheetPng`"", ([Uri]$sheetHtml).AbsoluteUri)
Start-Process -FilePath $browser -ArgumentList $arguments -Wait -WindowStyle Hidden
for ($i = 0; $i -lt 50 -and -not (Test-Path $sheetPng); $i++) { Start-Sleep -Milliseconds 100 }
if (-not (Test-Path $sheetPng)) { throw "$browser で SVG を描けませんでした。" }

$sheet = [Drawing.Bitmap]::FromFile($sheetPng)
function Get-Image([int]$Size) {
    $bitmap = New-Object Drawing.Bitmap $Size, $Size, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $g.DrawImage($sheet, (New-Object Drawing.Rectangle 0, 0, $Size, $Size), (New-Object Drawing.Rectangle $places[$Size], 0, $Size, $Size), [Drawing.GraphicsUnit]::Pixel)
    $g.Dispose()
    return $bitmap
}

# 32 ビットの BMP（BITMAPINFOHEADER、下から上の BGRA、AND マスク）を .ico の 1 枚にする。
function Get-DibBytes([Drawing.Bitmap]$Bitmap) {
    $size = $Bitmap.Width
    $stream = New-Object IO.MemoryStream
    $writer = New-Object IO.BinaryWriter $stream
    $maskRow = [int]([Math]::Ceiling($size / 32.0) * 4)
    $writer.Write([int]40); $writer.Write([int]$size); $writer.Write([int]($size * 2))
    $writer.Write([int16]1); $writer.Write([int16]32); $writer.Write([int]0)
    $writer.Write([int]($size * $size * 4 + $maskRow * $size)); $writer.Write([int]0); $writer.Write([int]0); $writer.Write([int]0); $writer.Write([int]0)
    for ($y = $size - 1; $y -ge 0; $y--) {
        for ($x = 0; $x -lt $size; $x++) {
            $c = $Bitmap.GetPixel($x, $y)
            $writer.Write([byte]$c.B); $writer.Write([byte]$c.G); $writer.Write([byte]$c.R); $writer.Write([byte]$c.A)
        }
    }
    $writer.Write((New-Object byte[] ($maskRow * $size)))
    $writer.Flush()
    return $stream.ToArray()
}

function Get-PngBytes([Drawing.Bitmap]$Bitmap) {
    $stream = New-Object IO.MemoryStream
    $Bitmap.Save($stream, [Drawing.Imaging.ImageFormat]::Png)
    return $stream.ToArray()
}

# .ico: ICONDIR、ICONDIRENTRY を並べ、そのあとに絵。
$images = foreach ($size in $icoSizes) {
    $bitmap = Get-Image $size
    $bytes = if ($size -ge 256) { Get-PngBytes $bitmap } else { Get-DibBytes $bitmap }
    $bitmap.Dispose()
    , @($size, $bytes)
}
$ico = New-Object IO.MemoryStream
$w = New-Object IO.BinaryWriter $ico
$w.Write([int16]0); $w.Write([int16]1); $w.Write([int16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($image in $images) {
    $size, $bytes = $image
    $edge = if ($size -ge 256) { 0 } else { $size }
    $w.Write([byte]$edge); $w.Write([byte]$edge); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([int16]1); $w.Write([int16]32); $w.Write([int]$bytes.Length); $w.Write([int]$offset)
    $offset += $bytes.Length
}
foreach ($image in $images) { $w.Write([byte[]]$image[1]) }
$w.Flush()
[IO.File]::WriteAllBytes((Join-Path $root 'assets\sqltrace.ico'), $ico.ToArray())
Write-Host "  assets\sqltrace.ico ($($icoSizes -join ', ') px, $($ico.Length) bytes)"

# GUI の窓のアイコン: 64x64 の RGBA。
$bitmap = Get-Image 64
$rgba = New-Object byte[] (64 * 64 * 4)
for ($y = 0; $y -lt 64; $y++) {
    for ($x = 0; $x -lt 64; $x++) {
        $c = $bitmap.GetPixel($x, $y)
        $at = ($y * 64 + $x) * 4
        $rgba[$at] = $c.R; $rgba[$at + 1] = $c.G; $rgba[$at + 2] = $c.B; $rgba[$at + 3] = $c.A
    }
}
$bitmap.Dispose()
[IO.File]::WriteAllBytes((Join-Path $root 'assets\sqltrace-64.rgba'), $rgba)
Write-Host '  assets\sqltrace-64.rgba (64 px)'

$bitmap = Get-Image 256
$bitmap.Save((Join-Path $root 'assets\sqltrace-256.png'), [Drawing.Imaging.ImageFormat]::Png)
$bitmap.Dispose()
Write-Host '  assets\sqltrace-256.png (256 px)'

$sheet.Dispose()
Remove-Item -Recurse -Force $work
