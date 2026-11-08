$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$assetDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'assets'
New-Item -ItemType Directory -Force -Path $assetDir | Out-Null
function RoundPath([single]$x,[single]$y,[single]$w,[single]$h,[single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x,$y,$d,$d,180,90); $p.AddArc(($x+$w-$d),$y,$d,$d,270,90)
    $p.AddArc(($x+$w-$d),($y+$h-$d),$d,$d,0,90); $p.AddArc($x,($y+$h-$d),$d,$d,90,90); $p.CloseFigure()
    return $p
}
function DrawIcon([int]$size) {
    $large = New-Object System.Drawing.Bitmap 1024,1024
    $g = [System.Drawing.Graphics]::FromImage($large)
    $g.SmoothingMode = 'AntiAlias'; $g.ScaleTransform(4,4)
    $colors = @('#102b3a','#f6be70','#eaf8f2','#17495a','#65dbc2')
    $brushes = @($colors | ForEach-Object { New-Object System.Drawing.SolidBrush([System.Drawing.ColorTranslator]::FromHtml($_)) })
    try {
        $p=RoundPath 8 8 240 240 54; $g.FillPath($brushes[0],$p); $p.Dispose()
        $g.TranslateTransform(125,138); $g.RotateTransform(-9); $g.TranslateTransform(-125,-138)
        $p=RoundPath 43 66 164 145 25; $g.FillPath($brushes[1],$p); $p.Dispose()
        $g.ResetTransform(); $g.ScaleTransform(4,4)
        $p=RoundPath 45 52 166 150 25; $g.FillPath($brushes[2],$p); $p.Dispose()
        $p=RoundPath 56 63 144 128 16; $g.FillPath($brushes[3],$p); $p.Dispose()
        $g.FillEllipse($brushes[4],79,78,98,98); $g.FillEllipse($brushes[0],95,94,66,66)
        $points=[System.Drawing.PointF[]]@([System.Drawing.PointF]::new(105,143),[System.Drawing.PointF]::new(124,117),[System.Drawing.PointF]::new(138,134),[System.Drawing.PointF]::new(150,123),[System.Drawing.PointF]::new(162,147),[System.Drawing.PointF]::new(105,147))
        $g.FillPolygon($brushes[2],$points); $g.FillEllipse($brushes[1],134,102,14,14)
        $spark=[System.Drawing.PointF[]]@([System.Drawing.PointF]::new(198,30),[System.Drawing.PointF]::new(203,46),[System.Drawing.PointF]::new(219,51),[System.Drawing.PointF]::new(203,56),[System.Drawing.PointF]::new(198,72),[System.Drawing.PointF]::new(193,56),[System.Drawing.PointF]::new(177,51),[System.Drawing.PointF]::new(193,46))
        $g.FillPolygon($brushes[1],$spark)
        $bmp = New-Object System.Drawing.Bitmap $size,$size
        $small=[System.Drawing.Graphics]::FromImage($bmp)
        try { $small.InterpolationMode='HighQualityBicubic'; $small.DrawImage($large,0,0,$size,$size) } finally { $small.Dispose() }
        return $bmp
    } finally { $g.Dispose(); $large.Dispose(); $brushes | ForEach-Object { $_.Dispose() } }
}
$sizes=@(16,24,32,48,64,128,256)
$images=@()
foreach($size in $sizes) {
    $bmp=DrawIcon $size
    try {
        $mem=New-Object System.IO.MemoryStream
        $bmp.Save($mem,[System.Drawing.Imaging.ImageFormat]::Png)
        $images+= ,$mem.ToArray(); $mem.Dispose()
        if($size -eq 256) { $bmp.Save((Join-Path $assetDir 'guangxia.png'),[System.Drawing.Imaging.ImageFormat]::Png) }
    } finally { $bmp.Dispose() }
}
$stream=[System.IO.File]::Create((Join-Path $assetDir 'guangxia.ico'))
$writer=New-Object System.IO.BinaryWriter($stream)
try {
    $writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$sizes.Count)
    $offset=6+16*$sizes.Count
    for($i=0;$i -lt $sizes.Count;$i++) {
        $n=if($sizes[$i] -eq 256){0}else{$sizes[$i]}
        $writer.Write([byte]$n);$writer.Write([byte]$n);$writer.Write([byte]0);$writer.Write([byte]0)
        $writer.Write([uint16]1);$writer.Write([uint16]32);$writer.Write([uint32]$images[$i].Length);$writer.Write([uint32]$offset)
        $offset+=$images[$i].Length
    }
    foreach($bytes in $images){$writer.Write([byte[]]$bytes)}
} finally {$writer.Dispose();$stream.Dispose()}
Write-Host '生成光匣多尺寸图标：16 / 24 / 32 / 48 / 64 / 128 / 256 px'
