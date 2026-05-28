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
      return
    fi
  done

  pids="$(lsof -nP -tiTCP:"$PORT" -sTCP:LISTEN || true)"
  if [ -n "$pids" ]; then
    kill -9 $pids 2>/dev/null || true
  fi
}

"$ROOT_DIR/scripts/prune-target.sh"
restart_fixed_port

cd "$ROOT_DIR"
exec env MAGI_WEB_DEV="${MAGI_WEB_DEV:-1}" MAGI_PORT="$PORT" cargo run -p magi-daemon-app
