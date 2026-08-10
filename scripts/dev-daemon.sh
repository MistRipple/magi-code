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
  BROWSER_CHROMIUM="${MAGI_BROWSER_DEV_CHROMIUM:-}"
  if [ -z "$BROWSER_CHROMIUM" ]; then
    BROWSER_RUNTIME_ROOT="${MAGI_STATE_ROOT:-$HOME/.magi}/runtimes/browser"
    if ACTIVE_RUNTIME_CHROMIUM="$(${MAGI_BROWSER_DEV_NODE:-node} - "$BROWSER_RUNTIME_ROOT" <<'NODE'
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(process.argv[2]);
const active = JSON.parse(fs.readFileSync(path.join(root, 'active.json'), 'utf8'));
const runtimeVersion = active.runtime_version;
if (typeof runtimeVersion !== 'string' || runtimeVersion.length === 0) process.exit(1);

const installRoot = path.resolve(root, runtimeVersion);
if (!installRoot.startsWith(`${root}${path.sep}`)) process.exit(1);
const manifest = JSON.parse(fs.readFileSync(path.join(installRoot, 'manifest.json'), 'utf8'));
if (manifest.runtime_version !== runtimeVersion) process.exit(1);

const executablePath = manifest.chromium_executable_path;
if (typeof executablePath !== 'string' || executablePath.length === 0) process.exit(1);
const executable = path.resolve(installRoot, executablePath);
if (!executable.startsWith(`${installRoot}${path.sep}`)) process.exit(1);
if (!fs.statSync(executable, { throwIfNoEntry: false })?.isFile()) process.exit(1);
process.stdout.write(executable);
NODE
    )"; then
      BROWSER_CHROMIUM="$ACTIVE_RUNTIME_CHROMIUM"
    fi
  fi
  if [ -z "$BROWSER_CHROMIUM" ] && [ -d "$BROWSER_NODE_MODULES/playwright-core" ]; then
    PLAYWRIGHT_CHROMIUM="$(${MAGI_BROWSER_DEV_NODE:-node} -e "const { chromium } = require('$BROWSER_NODE_MODULES/playwright-core'); process.stdout.write(chromium.executablePath())")"
    if [ -f "$PLAYWRIGHT_CHROMIUM" ]; then
      BROWSER_CHROMIUM="$PLAYWRIGHT_CHROMIUM"
    fi
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
  if [ ! -f "$BROWSER_HOST_ENTRY" ] || [ ! -f "$BROWSER_CHROMIUM" ]; then
    echo "workspace 模式缺少 Browser Host 或 Chromium：请设置 MAGI_BROWSER_DEV_HOST_ENTRY/MAGI_BROWSER_DEV_CHROMIUM。" >&2
    exit 1
  fi
  DAEMON_ENV+=(
    "MAGI_BROWSER_RUNTIME_MODE=workspace"
    "MAGI_BROWSER_DEV_HOST_ENTRY=$BROWSER_HOST_ENTRY"
    "MAGI_BROWSER_DEV_CHROMIUM=$BROWSER_CHROMIUM"
  )
elif [ "${MAGI_BROWSER_RUNTIME_MODE:-managed}" != "managed" ]; then
  echo "MAGI_BROWSER_RUNTIME_MODE 只能是 managed 或 workspace。" >&2
  exit 1
fi

exec env "${DAEMON_ENV[@]}" cargo run -p magi-daemon-app
