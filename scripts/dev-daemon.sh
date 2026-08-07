#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${MAGI_PORT:-38123}"

restart_fixed_port() {
  if ! command -v lsof >/dev/null 2>&1; then
    return
  fi

  local pids
  pids="$(lsof -nP -tiTCP:"$PORT" -sTCP:LISTEN || true)"
  if [ -z "$pids" ]; then
    return
  fi

  echo "端口 $PORT 已被占用，停止旧进程后重新启动。"
  kill $pids 2>/dev/null || true
  for _ in $(seq 1 20); do
    if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
      sleep 0.5
    else
      return 0
    fi
  done

  pids="$(lsof -nP -tiTCP:"$PORT" -sTCP:LISTEN || true)"
  if [ -n "$pids" ]; then
    kill -9 $pids 2>/dev/null || true
  fi

  return 0
}

"$ROOT_DIR/scripts/prune-target.sh"
restart_fixed_port

cd "$ROOT_DIR"
# Browser Host 通过 `dist/index.cjs` 启动。开发态必须先重新构建，避免 Rust
# 协议已升级而 Host 仍使用上一次构建产物，导致运行时握手不兼容。
npm --prefix browser-host run build

# 先编译 bridge loopback 二进制，daemon 运行时通过子进程方式拉起它们。
# cargo clean 后只编译 magi-daemon-app 会导致 bridge 可执行文件缺失。
cargo build -p magi-bridge-client --bins

DAEMON_ENV=(
  "MAGI_WEB_DEV=${MAGI_WEB_DEV:-1}"
  "MAGI_PORT=$PORT"
)

# 开发态优先复用 browser-host 自己声明的 Playwright Chromium，确保 Host 与
# Chromium 版本匹配；显式环境变量仍可覆盖自动发现结果。
BROWSER_HOST_ENTRY="${MAGI_BROWSER_DEV_HOST_ENTRY:-$ROOT_DIR/browser-host/dist/index.cjs}"
BROWSER_NODE_MODULES="$ROOT_DIR/browser-host/node_modules"
BROWSER_CHROMIUM="${MAGI_BROWSER_DEV_CHROMIUM:-}"
if [ -z "$BROWSER_CHROMIUM" ] && [ -d "$BROWSER_NODE_MODULES/playwright-core" ]; then
  BROWSER_CHROMIUM="$(${MAGI_BROWSER_DEV_NODE:-node} -e "const { chromium } = require('$BROWSER_NODE_MODULES/playwright-core'); process.stdout.write(chromium.executablePath())")"
fi
if [ -z "$BROWSER_CHROMIUM" ] && [ "$(uname -s)" = "Darwin" ]; then
  for candidate in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium"; do
    if [ -f "$candidate" ]; then
      BROWSER_CHROMIUM="$candidate"
      break
    fi
  done
fi
if [ -z "$BROWSER_CHROMIUM" ]; then
  for candidate in google-chrome-stable google-chrome chromium chromium-browser; do
    if command -v "$candidate" >/dev/null 2>&1; then
      BROWSER_CHROMIUM="$(command -v "$candidate")"
      break
    fi
  done
fi
if [ -f "$BROWSER_HOST_ENTRY" ] && [ -f "$BROWSER_CHROMIUM" ]; then
  DAEMON_ENV+=(
    "MAGI_BROWSER_DEV_HOST_ENTRY=$BROWSER_HOST_ENTRY"
    "MAGI_BROWSER_DEV_CHROMIUM=$BROWSER_CHROMIUM"
  )
fi

exec env "${DAEMON_ENV[@]}" cargo run -p magi-daemon-app
