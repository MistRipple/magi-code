#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${MAGI_PORT:-38123}"
PRODUCT_VERSION="${MAGI_PRODUCT_VERSION:-$(node "$ROOT_DIR/scripts/product-version.mjs")}"
BUILD_ID="${MAGI_BUILD_ID:-$(git -C "$ROOT_DIR" rev-parse HEAD)}"
SERVICE_NAME="${MAGI_SERVICE_NAME:-magi-rust-backend}"

restart_fixed_port() {
  if ! command -v lsof >/dev/null 2>&1; then
    return
  fi

  local pids
  pids="$(lsof -nP -tiTCP:"$PORT" -sTCP:LISTEN || true)"
  if [ -z "$pids" ]; then
    return
  fi

  is_magi_process() {
    local pid="$1"
    local command_line
    command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    case "$command_line" in
      *magi-daemon-app*|*"cargo run -p magi-daemon-app"*) return 0 ;;
      *) return 1 ;;
    esac
  }

  for pid in $pids; do
    if ! is_magi_process "$pid"; then
      echo "端口 $PORT 已被非 Magi 进程占用（PID $pid），拒绝终止该进程。" >&2
      return 1
    fi
  done

  echo "端口 $PORT 已被 Magi daemon 占用，停止旧进程后重新启动。"
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
    for pid in $pids; do
      if ! is_magi_process "$pid"; then
        echo "端口 $PORT 的监听进程已变化且不是 Magi，拒绝强制终止（PID $pid）。" >&2
        return 1
      fi
    done
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
  "MAGI_SERVICE_NAME=$SERVICE_NAME"
  "MAGI_PRODUCT_VERSION=$PRODUCT_VERSION"
  "MAGI_BUILD_ID=$BUILD_ID"
)

exec env "${DAEMON_ENV[@]}" cargo run -p magi-daemon-app
