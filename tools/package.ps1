# 光匣 · 本地影像管家 —— 交付打包
#
#   powershell -ExecutionPolicy Bypass -File tools\package.ps1
#   powershell -ExecutionPolicy Bypass -File tools\package.ps1 -SkipBuild     # 复用已有 exe
#   powershell -ExecutionPolicy Bypass -File tools\package.ps1 -Msix         # 额外尝试出 MSIX
#
# 产物：
#   dist\Guangxia-<版本>-win-x64\        便携目录（双击 启动.cmd 即用）
#   dist\Guangxia-<版本>-win-x64.zip     压缩包，可直接拷走
#   dist\Guangxia-<版本>-win-x64.msix    仅在 -Msix 且签名可用时生成

param(
    [string]$Version = '',
    [switch]$SkipBuild,
    [switch]$RefreshPackage,
    [switch]$Msix
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$cargoText = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
$cargoVersion = [regex]::Match($cargoText, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $Version) { $Version = $cargoVersion }
if ($Version -ne $cargoVersion) { throw "打包版本 $Version 与程序版本 $cargoVersion 不一致" }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw '版本号格式不正确' }
$release = Join-Path $root 'target\release'
$exeSrc = Join-Path $release 'photo-video-manager.exe'
$distRoot = Join-Path $root 'dist'
$name = "Guangxia-$Version-win-x64"
$out = Join-Path $distRoot $name

if ((Test-Path -LiteralPath $out) -and !$RefreshPackage) {
    throw "交付目录已存在：$out。请先保留或移走旧包，避免删除其中的用户数据。"
}

# 仓库与交付目录都保存 7z 组件，不提交解压后超过 GitHub 限制的 exe。
$libSrc = Join-Path $root 'lib'
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'tools\compress-ffmpeg.ps1')
if ($LASTEXITCODE -ne 0) { throw '准备内置 FFmpeg 压缩组件失败，已停止打包' }

if (-not $SkipBuild) {
    Write-Host '编译 release…' -ForegroundColor Cyan
    Push-Location $root
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build 失败' }
    } finally { Pop-Location }
}
if (-not (Test-Path -LiteralPath $exeSrc)) { throw "找不到 $exeSrc，请先编译" }

# ---------- 便携目录 ----------
# 显式刷新时仅更新程序管理的文件，保留 data、exports 及其他本地内容。
New-Item -ItemType Directory -Force -Path $out | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $out 'lib') | Out-Null

Copy-Item -LiteralPath $exeSrc -Destination (Join-Path $out 'Guangxia.exe')
$sizeExe = (Get-Item -LiteralPath $exeSrc).Length
Copy-Item -LiteralPath (Join-Path $root 'assets/guangxia.ico') -Destination (Join-Path $out 'Guangxia.ico') -Force

# HEIC 解码器（可选组件）：找不到 lib\ 时程序照常启动，只是 HEIC 无缩略图
$libSrc = Join-Path $root 'lib'
if (Test-Path -LiteralPath $libSrc) {
    Copy-Item -Path (Join-Path $libSrc '*.dll') -Destination (Join-Path $out 'lib') -Force
    $ffmpeg = Join-Path $libSrc 'ffmpeg.7z'
    if (-not (Test-Path -LiteralPath $ffmpeg)) { throw '缺少内置 FFmpeg：lib\ffmpeg.7z，无法交付完整视频功能' }
    Copy-Item -LiteralPath $ffmpeg -Destination (Join-Path $out 'lib') -Force
    Copy-Item -LiteralPath (Join-Path $libSrc 'ffmpeg.sha256') -Destination (Join-Path $out 'lib') -Force
    Copy-Item -LiteralPath (Join-Path $libSrc 'README-FFmpeg.txt') -Destination (Join-Path $out 'lib') -Force
    Copy-Item -Path (Join-Path $libSrc 'LICENSE-*.txt') -Destination (Join-Path $out 'lib') -Force
} else {
    Write-Host '警告：没有 lib\ 目录，包内将不含 HEIC 解码器' -ForegroundColor Yellow
}

Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $out -Force
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $out -Force
if (Test-Path -LiteralPath (Join-Path $root 'TODO.md')) {
    Copy-Item -LiteralPath (Join-Path $root 'TODO.md') -Destination $out -Force
}

# 启动器：不依赖 PowerShell 执行策略，双击即用
$launcher = @'
@echo off
chcp 65001 >nul
start "" "%~dp0Guangxia.exe" %*
'@
Set-Content -LiteralPath (Join-Path $out '启动.cmd') -Value $launcher -Encoding ASCII

# 第三方组件说明（LGPL 要求可替换的动态库）
$notices = @"
光匣 · 本地影像管家 $Version

本目录包含以下第三方组件（动态链接，可自行替换同名 DLL）：
  lib\heif.dll      libheif 1.23.7   LGPL-3.0   详见 lib\LICENSE-libheif.txt
  lib\libde265.dll  libde265 1.0.15  LGPL-3.0   详见 lib\LICENSE-libde265.txt
  lib\ffmpeg.7z     FFmpeg（Gyan Windows essentials build）GPL-3.0-or-later
                   首次启动自动离线解压，SHA-256 校验后使用。
                   版本、构建配置及源码链接：lib\README-FFmpeg.txt
                   许可证：lib\LICENSE-FFmpeg.txt

这两个 DLL 仅用于 HEIC/HEIF 解码，可以整体删除：程序仍可正常运行，
只是 HEIC 文件会退化为"只索引、无缩略图"。

程序本身不联网、不需要账号。扫描不修改原文件；明确选择覆盖或移动操作时会修改文件。
"@
Set-Content -LiteralPath (Join-Path $out 'THIRD-PARTY-NOTICES.txt') -Value $notices -Encoding UTF8

# ---------- 压缩包 ----------
# 用 -Force 覆盖旧包，避免先删除（某些环境会拦截删除操作）
$zip = "$out.zip"
$zipStage = Join-Path $root ('.cache\package\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path (Join-Path $zipStage 'lib') | Out-Null
foreach ($file in @('Guangxia.exe','Guangxia.ico','启动.cmd','README.md','LICENSE','TODO.md','THIRD-PARTY-NOTICES.txt')) {
    $source = Join-Path $out $file
    if (Test-Path -LiteralPath $source) { Copy-Item -LiteralPath $source -Destination $zipStage }
}
Get-ChildItem -LiteralPath (Join-Path $out 'lib') -File | Where-Object {
    $_.Extension -eq '.dll' -or $_.Name -like 'LICENSE-*.txt' -or
    $_.Name -in @('ffmpeg.7z','ffmpeg.sha256','README-FFmpeg.txt')
} | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $zipStage 'lib') }
try {
    Compress-Archive -Path (Join-Path $zipStage '*') -DestinationPath $zip -CompressionLevel Optimal -Force
} finally {
    $stageAbsolute = [IO.Path]::GetFullPath($zipStage)
    $allowedStageRoot = [IO.Path]::GetFullPath((Join-Path $root '.cache\package')) + [IO.Path]::DirectorySeparatorChar
    if (!$stageAbsolute.StartsWith($allowedStageRoot,[StringComparison]::OrdinalIgnoreCase)) { throw '打包临时目录越界，拒绝清理' }
    Remove-Item -LiteralPath $stageAbsolute -Recurse -Force
}

# ---------- MSIX（可选，无签名证书时只出包不签名） ----------
$msixPath = "$out.msix"
if ($Msix) {
    $makeappx = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\makeappx.exe' -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $makeappx) {
        Write-Host '未找到 makeappx.exe（需要 Windows SDK），跳过 MSIX' -ForegroundColor Yellow
    } else {
        $stage = Join-Path $distRoot "msix-stage-$Version"
        if (Test-Path -LiteralPath $stage) {
            try { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction Stop } catch { Write-Host '（旧暂存目录未清空，覆盖写入）' -ForegroundColor Yellow }
        }
        Copy-Item -LiteralPath $out -Destination $stage -Recurse
        # MSIX 包内不要 .cmd 启动器与说明文件
        Remove-Item -LiteralPath (Join-Path $stage '启动.cmd') -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $stage 'THIRD-PARTY-NOTICES.txt') -Force -ErrorAction SilentlyContinue

        $manifest = @"
<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         IgnorableNamespaces="uap rescap">
  <Identity Name="Huatek.PhotoVideoManager" Publisher="CN=PhotoVideoManager" Version="$Version.0" ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>光匣 · 本地影像管家</DisplayName>
    <PublisherDisplayName>Huatek</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.17763.0" MaxVersionTested="10.0.22621.0" />
  </Dependencies>
  <Resources><Resource Language="zh-cn" /></Resources>
  <Applications>
    <Application Id="App" Executable="Guangxia.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="光匣 · 本地影像管家" Description="离线、非破坏性的本地照片与视频整理工具"
        BackgroundColor="transparent" Square150x150Logo="Assets\Square150x150Logo.png" Square44x44Logo="Assets\Square44x44Logo.png" />
    </Application>
  </Applications>
  <Capabilities>
    <rescap:Capability Name="runFullTrust" />
  </Capabilities>
</Package>
"@
        Set-Content -LiteralPath (Join-Path $stage 'AppxManifest.xml') -Value $manifest -Encoding UTF8

        # 必需图标：用一张纯色 PNG 生成三种尺寸
        $assets = Join-Path $stage 'Assets'
        New-Item -ItemType Directory -Force -Path $assets | Out-Null
        Add-Type -AssemblyName System.Drawing
        foreach ($pair in @(@('StoreLogo.png', 50), @('Square150x150Logo.png', 150), @('Square44x44Logo.png', 44))) {
            $bmp = New-Object System.Drawing.Bitmap $pair[1], $pair[1]
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $brandIcon = [System.Drawing.Image]::FromFile((Join-Path $root 'assets/guangxia.png'))
            try { $g.InterpolationMode = 'HighQualityBicubic'; $g.DrawImage($brandIcon, 0, 0, [int]$pair[1], [int]$pair[1]) } finally { $brandIcon.Dispose() }
            $g.Dispose()
            $bmp.Save((Join-Path $assets $pair[0]), [System.Drawing.Imaging.ImageFormat]::Png)
            $bmp.Dispose()
        }

        # makeappx /o 会覆盖已有包，无需先删
        & $makeappx.FullName pack /d $stage /p $msixPath /o | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Write-Host "MSIX 已生成：$msixPath（未签名，安装前需自签证书或用 Add-AppDevPackage）" -ForegroundColor Yellow
        } else {
            Write-Host 'makeappx 打包失败，跳过 MSIX' -ForegroundColor Yellow
        }
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ---------- 汇总 ----------
function Fmt([long]$b) { '{0:N1} MB' -f ($b / 1MB) }
$dllBytes = (Get-ChildItem (Join-Path $out 'lib') -File -Filter *.dll | Measure-Object -Property Length -Sum).Sum
Write-Host ''
Write-Host "交付目录：$out" -ForegroundColor Green
Write-Host ("  主程序 {0} + 解码器 {1}" -f (Fmt $sizeExe), (Fmt $dllBytes))
Write-Host ("  压缩包 {0}（{1}）" -f $zip, (Fmt (Get-Item -LiteralPath $zip).Length))
if (Test-Path -LiteralPath $msixPath) {
    Write-Host ("  MSIX   {0}（{1}）" -f $msixPath, (Fmt (Get-Item -LiteralPath $msixPath).Length))
}
