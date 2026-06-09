#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PLATFORM="${1:?缺少平台名称，例如 macos 或 linux}"
ARCH="${MAGI_PACKAGE_ARCH:-$(uname -m)}"
VERSION="$(awk '
  /^\[workspace.package\]/ { in_workspace_package = 1; next }
  /^\[/ { in_workspace_package = 0 }
  in_workspace_package && /^version = / {
    gsub(/"/, "", $3)
    print $3
    exit
  }
' "$ROOT_DIR/Cargo.toml")"

if [ -z "$VERSION" ]; then
  echo "无法从 Cargo.toml 读取 workspace 版本号。" >&2
  exit 1
fi

BINARY="$ROOT_DIR/target/release/magi-daemon-app"
WEB_DIST="$ROOT_DIR/web/dist"
ICON_SVG="$ROOT_DIR/resources/package/magi.svg"
DIST_DIR="$ROOT_DIR/dist"
PACKAGE_NAME="magi-${VERSION}-${PLATFORM}-${ARCH}"
PACKAGE_DIR="$DIST_DIR/$PACKAGE_NAME"

if [ ! -x "$BINARY" ]; then
  echo "缺少 release daemon 二进制：$BINARY" >&2
  exit 1
fi

if [ ! -f "$WEB_DIST/web.html" ]; then
  echo "缺少前端构建产物：$WEB_DIST/web.html" >&2
  exit 1
fi

if [ ! -f "$ICON_SVG" ]; then
  echo "缺少产品图标资源：$ICON_SVG" >&2
  exit 1
fi

mkdir -p "$DIST_DIR"
rm -rf "$PACKAGE_DIR"

forbid_technical_entries() {
  local root="$1"
  if find "$root" \( -name 'magi-daemon-app*' -o -name 'start-magi*' \) | grep -q .; then
    echo "产品包不能暴露 magi-daemon-app 或 start-magi 技术入口。" >&2
    exit 1
  fi
}

package_macos() {
  local app_dir="$PACKAGE_DIR/Magi.app"
  local contents_dir="$app_dir/Contents"
  local macos_dir="$contents_dir/MacOS"
  local resources_dir="$contents_dir/Resources"
  local dmg_path="$DIST_DIR/$PACKAGE_NAME.dmg"

  mkdir -p "$macos_dir" "$resources_dir/web"
  cp "$BINARY" "$macos_dir/Magi"
  cp -R "$WEB_DIST" "$resources_dir/web/dist"
  chmod +x "$macos_dir/Magi"

  cat > "$contents_dir/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>zh_CN</string>
  <key>CFBundleDisplayName</key>
  <string>Magi</string>
  <key>CFBundleExecutable</key>
  <string>Magi</string>
  <key>CFBundleIdentifier</key>
  <string>com.mistripple.magi</string>
  <key>CFBundleName</key>
  <string>Magi</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

  if [ ! -x "$macos_dir/Magi" ]; then
    echo "Magi.app 缺少可执行入口：$macos_dir/Magi" >&2
    exit 1
  fi

  if [ ! -f "$resources_dir/web/dist/web.html" ]; then
    echo "Magi.app 缺少内置 UI 入口：$resources_dir/web/dist/web.html" >&2
    exit 1
  fi

  forbid_technical_entries "$app_dir"

  if ! command -v hdiutil >/dev/null 2>&1; then
    echo "macOS 产品包需要 hdiutil 生成 dmg。" >&2
    exit 1
  fi

  rm -f "$dmg_path"
  hdiutil create -volname "Magi" -srcfolder "$PACKAGE_DIR" -ov -format UDZO "$dmg_path"

  if [ ! -f "$dmg_path" ]; then
    echo "未生成 macOS dmg：$dmg_path" >&2
    exit 1
  fi

  echo "已生成 $dmg_path"
}

appimage_arch() {
  case "$ARCH" in
    x86_64 | amd64)
      echo "x86_64"
      ;;
    aarch64 | arm64)
      echo "aarch64"
      ;;
    *)
      echo "不支持的 AppImage 架构：$ARCH" >&2
      exit 1
      ;;
  esac
}

ensure_appimagetool() {
  local tool_arch="$1"
  local configured="${MAGI_APPIMAGETOOL:-}"
  if [ -n "$configured" ]; then
    if [ ! -x "$configured" ]; then
      echo "MAGI_APPIMAGETOOL 不可执行：$configured" >&2
      exit 1
    fi
    printf '%s\n' "$configured"
    return
  fi

  local tool_dir="$ROOT_DIR/target/package-tools"
  local tool_path="$tool_dir/appimagetool-${tool_arch}.AppImage"
  if [ ! -x "$tool_path" ]; then
    mkdir -p "$tool_dir"
    if ! command -v curl >/dev/null 2>&1; then
      echo "缺少 curl，无法下载 appimagetool。" >&2
      exit 1
    fi
    curl -fsSL \
      "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${tool_arch}.AppImage" \
      -o "$tool_path"
    chmod +x "$tool_path"
  fi
  printf '%s\n' "$tool_path"
}

package_linux() {
  local appdir="$PACKAGE_DIR/Magi.AppDir"
  local appimage_path="$DIST_DIR/$PACKAGE_NAME.AppImage"
  local tool_arch
  local appimagetool

  mkdir -p \
    "$appdir/usr/bin" \
    "$appdir/usr/share/applications" \
    "$appdir/usr/share/icons/hicolor/scalable/apps" \
    "$appdir/usr/share/magi/web"

  cp "$BINARY" "$appdir/usr/bin/Magi"
  cp -R "$WEB_DIST" "$appdir/usr/share/magi/web/dist"
  cp "$ICON_SVG" "$appdir/magi.svg"
  cp "$ICON_SVG" "$appdir/.DirIcon"
  cp "$ICON_SVG" "$appdir/usr/share/icons/hicolor/scalable/apps/magi.svg"
  chmod +x "$appdir/usr/bin/Magi"

  cat > "$appdir/magi.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Magi
Comment=Local AI coding workspace
Exec=Magi
Icon=magi
Categories=Development;Utility;
Terminal=false
DESKTOP
  cp "$appdir/magi.desktop" "$appdir/usr/share/applications/magi.desktop"

  cat > "$appdir/AppRun" <<'APPRUN'
#!/usr/bin/env sh
set -eu

APPDIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
export MAGI_WEB_DIST_ROOT="${MAGI_WEB_DIST_ROOT:-$APPDIR/usr/share/magi/web/dist}"
exec "$APPDIR/usr/bin/Magi" "$@"
APPRUN
  chmod +x "$appdir/AppRun"

  if [ ! -x "$appdir/AppRun" ]; then
    echo "AppDir 缺少 AppRun 入口：$appdir/AppRun" >&2
    exit 1
  fi

  if [ ! -x "$appdir/usr/bin/Magi" ]; then
    echo "AppDir 缺少 Magi 可执行入口：$appdir/usr/bin/Magi" >&2
    exit 1
  fi

  if [ ! -f "$appdir/usr/share/magi/web/dist/web.html" ]; then
    echo "AppDir 缺少内置 UI 入口：$appdir/usr/share/magi/web/dist/web.html" >&2
    exit 1
  fi

  forbid_technical_entries "$appdir"

  tool_arch="$(appimage_arch)"
  appimagetool="$(ensure_appimagetool "$tool_arch")"

  rm -f "$appimage_path"
  ARCH="$tool_arch" APPIMAGE_EXTRACT_AND_RUN=1 "$appimagetool" "$appdir" "$appimage_path"
  chmod +x "$appimage_path"

  if [ ! -x "$appimage_path" ]; then
    echo "未生成 Linux AppImage：$appimage_path" >&2
    exit 1
  fi

  echo "已生成 $appimage_path"
}

case "$PLATFORM" in
  macos)
    package_macos
    ;;
  linux)
    package_linux
    ;;
  *)
    echo "不支持的平台：$PLATFORM" >&2
    exit 1
    ;;
esac
