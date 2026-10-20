#!/usr/bin/env bash
set -euo pipefail

# 获取当前项目根目录
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OS="$(uname -s)"
ARCH="$(uname -m)"

VERSION="chromium/7000" # 稳定版本标记或最新 release

case "$OS" in
  Darwin*)
    if [ "$ARCH" = "arm64" ]; then
      TARGET="mac-arm64"
    else
      TARGET="mac-x64"
    fi
    LIB_NAME="libpdfium.dylib"
    ;;
  Linux*)
    if [ "$ARCH" = "aarch64" ] || [ "$ARCH" = "arm64" ]; then
      TARGET="linux-arm64"
    else
      TARGET="linux-x64"
    fi
    LIB_NAME="libpdfium.so"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    TARGET="win-x64"
    LIB_NAME="pdfium.dll"
    ;;
  *)
    echo "不支持的操作系统: $OS ($ARCH)"
    exit 1
    ;;
esac

echo "检测到环境: $OS ($ARCH)，目标平台: $TARGET"

if [ -f "$ROOT_DIR/$LIB_NAME" ]; then
  echo "$LIB_NAME 已存在于项目根目录，无需重复下载。"
  exit 0
fi

DOWNLOAD_URL="https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-${TARGET}.tgz"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "正在从 GitHub 下载 PDFium 二进制 ($TARGET)..."
echo "URL: $DOWNLOAD_URL"

if command -v curl >/dev/null 2>&1; then
  curl -fsSL -o "$TMP_DIR/pdfium.tgz" "$DOWNLOAD_URL"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$TMP_DIR/pdfium.tgz" "$DOWNLOAD_URL"
else
  echo "请先安装 curl 或 wget"
  exit 1
fi

echo "正在解压..."
tar -xzf "$TMP_DIR/pdfium.tgz" -C "$TMP_DIR"

if [ -f "$TMP_DIR/lib/$LIB_NAME" ]; then
  cp "$TMP_DIR/lib/$LIB_NAME" "$ROOT_DIR/$LIB_NAME"
elif [ -f "$TMP_DIR/bin/$LIB_NAME" ]; then
  cp "$TMP_DIR/bin/$LIB_NAME" "$ROOT_DIR/$LIB_NAME"
else
  echo "解压包中未找到 $LIB_NAME"
  exit 1
fi

echo "成功安装 $LIB_NAME 到 $ROOT_DIR !"
