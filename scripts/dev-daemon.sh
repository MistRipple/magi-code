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
# 先编译 bridge loopback 二进制，daemon 运行时通过子进程方式拉起它们。
# cargo clean 后只编译 magi-daemon-app 会导致 bridge 可执行文件缺失。
cargo build -p magi-bridge-client --bins

DAEMON_ENV=(
  "MAGI_WEB_DEV=${MAGI_WEB_DEV:-1}"
  "MAGI_PORT=$PORT"
)

if [ "${MAGI_BROWSER_RUNTIME_MODE:-managed}" = "workspace" ]; then
  # workspace 模式只用于开发 Browser Host 协议。默认 managed 模式必须使用
  # 与正式包相同的签名 Runtime，确保设置页的安装、更新、卸载链路可验证。
  npm --prefix browser-host run build

  BROWSER_HOST_ENTRY="${MAGI_BROWSER_DEV_HOST_ENTRY:-$ROOT_DIR/browser-host/dist/index.cjs}"
  BROWSER_NODE_MODULES="$ROOT_DIR/browser-host/node_modules"
  BROWSER_PLAYWRIGHT_VERSION="$(${MAGI_BROWSER_DEV_NODE:-node} -p "require('$ROOT_DIR/browser-host/package.json').dependencies['playwright-core']")"
  BROWSER_EXPECTED_CHROMIUM_VERSION="$(${MAGI_BROWSER_DEV_NODE:-node} -e "const manifest = require('$BROWSER_NODE_MODULES/playwright-core/browsers.json'); const chromium = manifest.browsers.find((entry) => entry.name === 'chromium'); if (!chromium?.browserVersion) process.exit(1); process.stdout.write(chromium.browserVersion)")"
  BROWSER_CHROMIUM="${MAGI_BROWSER_DEV_CHROMIUM:-}"
  if [ -z "$BROWSER_CHROMIUM" ] && [ -d "$BROWSER_NODE_MODULES/playwright-core" ]; then
    PLAYWRIGHT_CHROMIUM="$(${MAGI_BROWSER_DEV_NODE:-node} -e "const { chromium } = require('$BROWSER_NODE_MODULES/playwright-core'); process.stdout.write(chromium.executablePath())")"
    if [ -f "$PLAYWRIGHT_CHROMIUM" ]; then
      BROWSER_CHROMIUM="$PLAYWRIGHT_CHROMIUM"
    fi
  fi
  if [ ! -f "$BROWSER_HOST_ENTRY" ] || [ ! -f "$BROWSER_CHROMIUM" ]; then
    echo "workspace 模式缺少与 Playwright $BROWSER_PLAYWRIGHT_VERSION 匹配的 Chromium。" >&2
    echo "请执行 npm --prefix browser-host exec -- playwright-core install chromium，或显式设置 MAGI_BROWSER_DEV_CHROMIUM。" >&2
    exit 1
  fi
  BROWSER_ACTUAL_CHROMIUM_VERSION="$("$BROWSER_CHROMIUM" --version | sed -E 's/[^0-9]*([0-9]+(\.[0-9]+){1,3}).*/\1/')"
  if [ "${BROWSER_ACTUAL_CHROMIUM_VERSION%%.*}" != "${BROWSER_EXPECTED_CHROMIUM_VERSION%%.*}" ]; then
    echo "workspace 模式 Chromium 协议版本不匹配：当前 $BROWSER_ACTUAL_CHROMIUM_VERSION，Playwright $BROWSER_PLAYWRIGHT_VERSION 需要 $BROWSER_EXPECTED_CHROMIUM_VERSION。" >&2
    exit 1
  fi
  DAEMON_ENV+=(
    "MAGI_BROWSER_RUNTIME_MODE=workspace"
    "MAGI_BROWSER_DEV_HOST_ENTRY=$BROWSER_HOST_ENTRY"
    "MAGI_BROWSER_DEV_CHROMIUM=$BROWSER_CHROMIUM"
    "MAGI_BROWSER_PLAYWRIGHT_VERSION=$BROWSER_PLAYWRIGHT_VERSION"
  )
elif [ "${MAGI_BROWSER_RUNTIME_MODE:-managed}" != "managed" ]; then
  echo "MAGI_BROWSER_RUNTIME_MODE 只能是 managed 或 workspace。" >&2
  exit 1
fi

exec env "${DAEMON_ENV[@]}" cargo run -p magi-daemon-app
