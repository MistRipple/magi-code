#!/usr/bin/env python3
import os
import sys
import time
import shutil
import subprocess
from pathlib import Path

def fmt(sz):
    for u in ['B', 'KB', 'MB', 'GB']:
        if sz < 1024:
            return f"{sz:.1f} {u}"
        sz /= 1024
    return f"{sz:.1f} TB"

def get_dir_size(path):
    if not path.exists():
        return 0
    total = 0
    for root, dirs, files in os.walk(path):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except Exception:
                pass
    return total

def safe_remove(p):
    sz = 0
    try:
        if p.is_symlink() or p.is_file():
            sz = p.stat().st_size
            p.unlink()
        elif p.is_dir():
            for root, dirs, files in os.walk(p):
                for f in files:
                    try:
                        sz += os.path.getsize(os.path.join(root, f))
                    except Exception:
                        pass
            shutil.rmtree(p)
    except Exception:
        pass
    return sz

def is_compiler_active():
    try:
        out = subprocess.run(
            ["pgrep", "-f", "rustc|cargo build|cargo test|cargo check|cargo clippy"],
            capture_output=True,
            text=True
        )
        pids = [int(p) for p in out.stdout.split() if p.strip()]
        return len(pids) > 0
    except Exception:
        return False

def main():
    root_dir = Path(__file__).resolve().parent.parent
    target_env = os.environ.get("CARGO_TARGET_DIR", "target")
    target_dir = Path(target_env) if os.path.isabs(target_env) else root_dir / target_env

    prune_gib = float(os.environ.get("MAGI_TARGET_PRUNE_GIB", "8"))
    force_clean = os.environ.get("MAGI_TARGET_FORCE_CLEAN", "0") == "1"
    prune_bytes = prune_gib * 1024 * 1024 * 1024

    # 临时工作区由创建者回收；构建缓存清理只操作指定 target。
    if not target_dir.exists():
        print(f"target 目录不存在: {target_dir}，跳过清理。")
        return

    cur_sz = get_dir_size(target_dir)
    print(f"target 缓存当前 {fmt(cur_sz)}，设定水位: {prune_gib:.1f} GiB。")

    if not force_clean and cur_sz < prune_bytes:
        print(f"target 缓存低于 {prune_gib:.1f} GiB 水位，无需修剪。")
        return

    # 若正在执行编译，则避免粗暴清理引发并发冲突，除非强制
    if is_compiler_active() and not force_clean:
        print("检测到后台有活跃的 cargo/rustc 编译/测试任务，为避免构建冲突，跳过本次修剪。")
        return

    print("开始智能修剪 target 缓存...")
    total_freed = 0

    # 级别 1: 清理增量编译缓存 (incremental) - 占比通常超过 40%，且对二次完整构建无破坏性
    for sub in ["debug/incremental", "release/incremental"]:
        inc_dir = target_dir / sub
        if inc_dir.exists():
            sz = safe_remove(inc_dir)
            total_freed += sz
            print(f"  已清理增量编译缓存: {sub} (释放 {fmt(sz)})")

    cur_sz = get_dir_size(target_dir)
    if not force_clean and cur_sz < prune_bytes:
        print(f"修剪完成！当前 target 大小为 {fmt(cur_sz)}，已回落至水位以下。")
        return

    # 级别 2: 清理 target/debug/deps 中的过期测试二进制文件（无后缀的可执行文件）
    deps_dir = target_dir / "debug/deps"
    if deps_dir.exists():
        test_bins = []
        now = time.time()
        for f in deps_dir.iterdir():
            if f.is_file() and not f.suffix and os.access(f, os.X_OK):
                test_bins.append(f)
        # 按修改时间从旧到新排序，清理超过 1 天或所有的旧测试可执行文件
        freed_bins = 0
        cleaned_bins = 0
        for f in test_bins:
            try:
                sz = safe_remove(f)
                freed_bins += sz
                cleaned_bins += 1
            except Exception:
                pass
        if cleaned_bins > 0:
            total_freed += freed_bins
            print(f"  已清理 {cleaned_bins} 个过期测试可执行程序 (释放 {fmt(freed_bins)})")

    cur_sz = get_dir_size(target_dir)
    if not force_clean and cur_sz < prune_bytes:
        print(f"修剪完成！当前 target 大小为 {fmt(cur_sz)}，已回落至水位以下。")
        return

    # 级别 3: 清理 Windows/其他交叉编译产物
    for item in target_dir.iterdir():
        if item.is_dir() and item.name.startswith("x86_64-"):
            sz = safe_remove(item)
            total_freed += sz
            print(f"  已清理交叉编译缓存: {item.name} (释放 {fmt(sz)})")

    cur_sz = get_dir_size(target_dir)
    if not force_clean and cur_sz < prune_bytes:
        print(f"修剪完成！当前 target 大小为 {fmt(cur_sz)}，已回落至水位以下。")
        return

    # 级别 4: 如果仍然超过阈值，或指定了 MAGI_TARGET_FORCE_CLEAN=1，执行 cargo clean
    print(f"修剪后 target 仍有 {fmt(cur_sz)}，执行完整 cargo clean...")
    subprocess.run(["cargo", "clean"], cwd=root_dir)
    new_sz = get_dir_size(target_dir)
    print(f"cargo clean 完成，当前 target 大小: {fmt(new_sz)}。")

if __name__ == "__main__":
    main()
