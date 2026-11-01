# 照片与视频管理器 —— 启动脚本
# 首次运行会自动编译（需要 Rust 1.88+ 与 MSVC 工具链），之后直接启动。
#
#   .\Start.ps1                    # 使用默认数据目录 %LOCALAPPDATA%\PhotoVideoManager
#   .\Start.ps1 -DataDir D:\pvm    # 指定数据目录（数据库、缩略图缓存、备份）
#   .\Start.ps1 -NoWindow          # 只启动本地服务、不打开窗口（调试用）

param(
    [string]$DataDir = '',
    [switch]$NoWindow
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$exe = Join-Path $root 'target\release\photo-video-manager.exe'

# HEIC 解码器（可选）：仓库根目录的 lib\ 需要跟可执行文件放在一起
$libSrc = Join-Path $root 'lib'
$libDst = Join-Path $root 'target\release\lib'
if (Test-Path -LiteralPath $libSrc) {
    $needCopy = -not (Test-Path -LiteralPath $libDst)
    if (-not $needCopy) {
        $srcStamp = (Get-ChildItem -LiteralPath $libSrc -File | Measure-Object -Property LastWriteTimeUtc -Maximum).Maximum
        $dstDll = Join-Path $libDst 'heif.dll'
        $needCopy = -not (Test-Path -LiteralPath $dstDll) -or $srcStamp -gt (Get-Item -LiteralPath $dstDll).LastWriteTimeUtc
    }
    if ($needCopy) {
        New-Item -ItemType Directory -Force -Path $libDst | Out-Null
        Copy-Item -Path (Join-Path $libSrc '*') -Destination $libDst -Force
    }
}

if (-not (Test-Path -LiteralPath $exe)) {
    Write-Host '首次运行：正在编译，请稍候…' -ForegroundColor Cyan
    Push-Location $root
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw '编译失败，请检查 Rust 工具链（rustup show）。' }
    } finally {
        Pop-Location
    }
}

$arguments = @()
if ($DataDir) { $arguments += @('--data-dir', $DataDir) }

if ($NoWindow) {
    # 调试用：留在当前控制台，日志直接打印
    $arguments += '--no-window'
    & $exe @arguments
    if ($LASTEXITCODE -ne 0) {
        Write-Host "程序退出，代码 $LASTEXITCODE" -ForegroundColor Yellow
    }
} else {
    # 发布版是 GUI 子系统：直接启动即可，不需要等它退出（窗口关掉程序就退了）
    Start-Process -FilePath $exe -ArgumentList $arguments | Out-Null
    Write-Host '已启动。窗口关闭后程序自动退出。' -ForegroundColor Green
}
