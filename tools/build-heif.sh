#!/usr/bin/env bash
# 用 MSVC 从源码构建「只带 libde265(HEVC 解码)」的最小 libheif，产物拷到 ./lib
# 不调用 vcvars64.bat（该脚本依赖被安全策略拦截的 reg.exe），改为手工拼 INCLUDE/LIB/PATH。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/.cache"
VS="C:\\Program Files\\Microsoft Visual Studio\\2022\\Community"
MSVC_WIN="$VS\\VC\\Tools\\MSVC\\14.42.34433"
SDK_WIN="C:\\Program Files (x86)\\Windows Kits\\10"
SDK_VER="10.0.22621.0"
CMAKE="/c/Program Files/Microsoft Visual Studio/2022/Community/Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe"
NINJA_DIR="/c/Program Files/Microsoft Visual Studio/2022/Community/Common7/IDE/CommonExtensions/Microsoft/CMake/Ninja"

export PATH="/c/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.42.34433/bin/Hostx64/x64:/c/Program Files (x86)/Windows Kits/10/bin/$SDK_VER/x64:$NINJA_DIR:$PATH"
export INCLUDE="$MSVC_WIN\\include;$SDK_WIN\\Include\\$SDK_VER\\ucrt;$SDK_WIN\\Include\\$SDK_VER\\um;$SDK_WIN\\Include\\$SDK_VER\\shared;$SDK_WIN\\Include\\$SDK_VER\\winrt;$SDK_WIN\\Include\\$SDK_VER\\cppwinrt"
export LIB="$MSVC_WIN\\lib\\x64;$SDK_WIN\\Lib\\$SDK_VER\\ucrt\\x64;$SDK_WIN\\Lib\\$SDK_VER\\um\\x64"

PREFIX="$(cygpath -w "$SRC/build/prefix")"

echo "[1/4] 配置 libde265"
"$CMAKE" -S "$SRC/libde265-1.0.15" -B "$SRC/build/de265" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON -DENABLE_SDL=OFF \
  -DCMAKE_INSTALL_PREFIX="$PREFIX"

echo "[2/4] 编译 libde265"
"$CMAKE" --build "$SRC/build/de265" --target install

echo "[3/4] 配置并编译 libheif（仅 libde265）"
"$CMAKE" -S "$SRC/libheif-1.23.7" -B "$SRC/build/heif" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON \
  -DWITH_LIBDE265=ON -DWITH_LIBDE265_PLUGIN=OFF \
  -DWITH_X265=OFF -DWITH_X264=OFF -DWITH_AOM_DECODER=OFF -DWITH_AOM_ENCODER=OFF \
  -DWITH_DAV1D=OFF -DWITH_RAV1E=OFF -DWITH_SvtEnc=OFF -DWITH_KVAZAAR=OFF \
  -DWITH_OPENH264=OFF -DWITH_UNCODEC=OFF \
  -DWITH_OPENJPEG_DECODER=OFF -DWITH_OPENJPEG_ENCODER=OFF -DWITH_OPENJPH_ENCODER=OFF \
  -DWITH_JPEG_DECODER=OFF -DWITH_JPEG_ENCODER=OFF -DWITH_LIBSHARPYUV=OFF -DWITH_BROTLI=OFF \
  -DWITH_EXAMPLES=OFF -DWITH_GDK_PIXBUF=OFF \
  -DLIBDE265_INCLUDE_DIR="$PREFIX\\include" -DLIBDE265_LIBRARY="$PREFIX\\lib\\de265.lib" \
  -DCMAKE_INSTALL_PREFIX="$PREFIX"
"$CMAKE" --build "$SRC/build/heif" --target install

echo "[4/4] 收集运行时 DLL"
mkdir -p "$ROOT/lib"
cp "$SRC/build/prefix/bin/heif.dll" "$ROOT/lib/heif.dll"
cp "$SRC/build/prefix/bin/libde265.dll" "$ROOT/lib/libde265.dll"
[ -f "$SRC/libde265-1.0.15/COPYING" ] && cp "$SRC/libde265-1.0.15/COPYING" "$ROOT/lib/LICENSE-libde265.txt"
[ -f "$SRC/libheif-1.23.7/COPYING" ] && cp "$SRC/libheif-1.23.7/COPYING" "$ROOT/lib/LICENSE-libheif.txt"
ls -l "$ROOT/lib"
