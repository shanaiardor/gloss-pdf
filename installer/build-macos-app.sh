#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="gloss-pdf"
VERSION="${APP_VERSION:-$(cargo metadata --no-deps --format-version 1 | grep -o '"version":"[^"]*"' | head -n1 | cut -d'"' -f4)}"
ARCH="$(uname -m)"
OUTPUT_DIR="$ROOT_DIR/dist"
APP_BUNDLE="$OUTPUT_DIR/${APP_NAME}.app"

echo "=== 开始打包 macOS App: ${APP_NAME} v${VERSION} ($ARCH) ==="

# 确保 libpdfium.dylib 存在
if [ ! -f "$ROOT_DIR/libpdfium.dylib" ]; then
    echo "未检测到 $ROOT_DIR/libpdfium.dylib，正在调用 scripts/setup-pdfium.sh 下载..."
    bash "$ROOT_DIR/scripts/setup-pdfium.sh"
fi

# 编译 release 二进制
echo "正在编译 Release 二进制..."
cargo build --release --locked -p "$APP_NAME"

# 清理并创建 bundle 目录结构
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Frameworks"
mkdir -p "$APP_BUNDLE/Contents/Resources"
mkdir -p "$OUTPUT_DIR"

# 拷贝可执行文件与动态库
cp "target/release/$APP_NAME" "$APP_BUNDLE/Contents/MacOS/$APP_NAME"
cp "$ROOT_DIR/libpdfium.dylib" "$APP_BUNDLE/Contents/Frameworks/libpdfium.dylib"
# 同时放一份在 MacOS 目录下兼容不同查找策略
cp "$ROOT_DIR/libpdfium.dylib" "$APP_BUNDLE/Contents/MacOS/libpdfium.dylib"

# 拷贝第三方声明和开源许可
cp "$ROOT_DIR/THIRD_PARTY_NOTICES.md" "$APP_BUNDLE/Contents/Resources/"
if [ -d "$ROOT_DIR/third_party" ]; then
    cp -r "$ROOT_DIR/third_party" "$APP_BUNDLE/Contents/Resources/"
fi

# 生成 Info.plist
cat <<EOF > "$APP_BUNDLE/Contents/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>zh_CN</string>
    <key>CFBundleExecutable</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>com.shanaiardor.gloss-pdf</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>笺释</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
EOF

# 配置动态链接库查找路径
chmod +x "$APP_BUNDLE/Contents/MacOS/$APP_NAME"
install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP_BUNDLE/Contents/MacOS/$APP_NAME" 2>/dev/null || true
install_name_tool -add_rpath "@executable_path" "$APP_BUNDLE/Contents/MacOS/$APP_NAME" 2>/dev/null || true

# 打包 dmg 和 zip
DMG_NAME="${APP_NAME}-${VERSION}-macos-${ARCH}.dmg"
ZIP_NAME="${APP_NAME}-${VERSION}-macos-${ARCH}.zip"

echo "正在生成 DMG 镜像: $DMG_NAME ..."
rm -f "$OUTPUT_DIR/$DMG_NAME"
hdiutil create -volname "gloss-pdf" -srcfolder "$APP_BUNDLE" -ov -format UDZO "$OUTPUT_DIR/$DMG_NAME"

echo "正在生成 ZIP 压缩包: $ZIP_NAME ..."
rm -f "$OUTPUT_DIR/$ZIP_NAME"
(cd "$OUTPUT_DIR" && zip -q -r "$ZIP_NAME" "${APP_NAME}.app")

echo "=== 打包完成! ==="
echo "产物路径:"
echo "  - $OUTPUT_DIR/$DMG_NAME"
echo "  - $OUTPUT_DIR/$ZIP_NAME"
