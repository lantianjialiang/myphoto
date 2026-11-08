# 固定版本、校验后提取随包 FFmpeg；只需要 Windows 自带的 curl 和 tar。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = '9.0.2'
$name = "ffmpeg-$version-essentials_build"
$stage = Join-Path $root '.cache\ffmpeg'
$archive = Join-Path $stage "$name.7z"
$expected = '4705843ccaaf54257c16ad90f3e952ece33c17df964ecf7bfdbb0f49c7171077'
New-Item -ItemType Directory -Force -Path $stage | Out-Null
if (!(Test-Path -LiteralPath $archive)) {
    & curl.exe -L --fail --retry 2 "https://github.com/GyanD/codexffmpeg/releases/download/$version/$name.7z" -o $archive
    if ($LASTEXITCODE -ne 0) { throw '下载 FFmpeg 失败' }
}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw 'FFmpeg 校验失败，请保留文件排查后重新下载' }
& tar.exe -xf $archive -C $stage
if ($LASTEXITCODE -ne 0) { throw '解压 FFmpeg 失败' }
$extracted = Join-Path $stage $name
$lib = Join-Path $root 'lib'
New-Item -ItemType Directory -Force -Path $lib | Out-Null
Copy-Item -LiteralPath (Join-Path $extracted 'bin\ffmpeg.exe') -Destination (Join-Path $lib 'ffmpeg.exe') -Force
Copy-Item -LiteralPath (Join-Path $extracted 'LICENSE') -Destination (Join-Path $lib 'LICENSE-FFmpeg.txt') -Force
Copy-Item -LiteralPath (Join-Path $extracted 'README.txt') -Destination (Join-Path $lib 'README-FFmpeg.txt') -Force
Write-Host "FFmpeg $version 已就绪：$lib"
