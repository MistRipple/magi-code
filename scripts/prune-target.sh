#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${CARGO_TARGET_DIR:-}" = /* ]]; then
  TARGET_DIR="$CARGO_TARGET_DIR"
else
  TARGET_DIR="$ROOT_DIR/${CARGO_TARGET_DIR:-target}"
fi
TARGET_PRUNE_GIB="${MAGI_TARGET_PRUNE_GIB:-8}"

target_size_kib() {
  if [ -d "$TARGET_DIR" ]; then
    du -sk "$TARGET_DIR" | awk '{print $1}'
  else
    echo 0
  fi
}

prune_kib=$((TARGET_PRUNE_GIB * 1024 * 1024))
size_kib="$(target_size_kib)"
size_gib=$((size_kib / 1024 / 1024))

if [ "${MAGI_TARGET_FORCE_CLEAN:-0}" = "1" ] || [ "$size_kib" -ge "$prune_kib" ]; then
  echo "target 缓存当前 ${size_gib}GiB，达到 ${TARGET_PRUNE_GIB}GiB 预清理水位，执行 cargo clean。"
  (cd "$ROOT_DIR" && cargo clean)
else
  echo "target 缓存当前 ${size_gib}GiB，低于 ${TARGET_PRUNE_GIB}GiB 预清理水位。"
fi
