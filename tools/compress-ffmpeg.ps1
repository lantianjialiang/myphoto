# Git 保存 7z 组件；运行时使用 Windows tar 离线解压。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$lib = Join-Path $root 'lib'
$binary = Join-Path $lib 'ffmpeg.exe'
$archive = Join-Path $lib 'ffmpeg.7z'
$manifest = Join-Path $lib 'ffmpeg.sha256'
if (!(Test-Path -LiteralPath $binary)) {
    if ((Test-Path -LiteralPath $archive) -and (Test-Path -LiteralPath $manifest)) { return }
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'fetch-ffmpeg.ps1')
    if ($LASTEXITCODE -ne 0) { throw '准备 FFmpeg 失败' }
}
$hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
if ((Test-Path -LiteralPath $archive) -and (Test-Path -LiteralPath $manifest) -and
    (Get-Content -LiteralPath $manifest -Raw).Trim().Split(' ')[0] -eq $hash) { return }
$sevenZip = @('C:\Program Files\7-Zip\7z.exe','C:\Program Files (x86)\7-Zip\7z.exe') |
    Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (!$sevenZip) { $sevenZip = (Get-Command 7z.exe -ErrorAction SilentlyContinue).Source }
if (!$sevenZip) { throw '重新生成 FFmpeg 压缩组件需要 7-Zip；仓库自带完整组件时无需安装' }
$temporary = Join-Path $lib ('.ffmpeg-' + [guid]::NewGuid().ToString('N') + '.7z')
Push-Location $lib
try {
    & $sevenZip a -t7z $temporary ffmpeg.exe -m0=LZMA2 -mx=7 -md=64m -mmt=2 -bsp0
    if ($LASTEXITCODE -ne 0) { throw '压缩 FFmpeg 失败' }
    & $sevenZip t $temporary -bsp0
    if ($LASTEXITCODE -ne 0) { throw 'FFmpeg 压缩完整性检查失败' }
    Move-Item -LiteralPath $temporary -Destination $archive -Force
    [IO.File]::WriteAllText($manifest, "$hash  ffmpeg.exe`n", [Text.Encoding]::ASCII)
} finally { Pop-Location }
