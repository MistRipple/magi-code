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

if [ ! -x "$BINARY" ]; then
  echo "缺少 release daemon 二进制：$BINARY" >&2
  exit 1
fi

if [ ! -f "$WEB_DIST/web.html" ]; then
  echo "缺少前端构建产物：$WEB_DIST/web.html" >&2
  exit 1
fi

DIST_DIR="$ROOT_DIR/dist"
PACKAGE_NAME="magi-${VERSION}-${PLATFORM}-${ARCH}"
PACKAGE_DIR="$DIST_DIR/$PACKAGE_NAME"

rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/resources/web"

cp "$BINARY" "$PACKAGE_DIR/Magi"
cp -R "$WEB_DIST" "$PACKAGE_DIR/resources/web/dist"

chmod +x "$PACKAGE_DIR/Magi"

mkdir -p "$DIST_DIR"
tar -C "$DIST_DIR" -czf "$DIST_DIR/$PACKAGE_NAME.tar.gz" "$PACKAGE_NAME"
echo "已生成 $DIST_DIR/$PACKAGE_NAME.tar.gz"
