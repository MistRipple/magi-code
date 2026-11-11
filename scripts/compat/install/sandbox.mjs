// 受限子进程：记录阶段为了读取清单而运行上游代码，必须在 Node 权限模型下进行。
// 权限模型（--permission）：默认拒绝网络、子进程、Worker、原生插件，文件系统只放行显式列出的路径。
// Node 官方文档说明它是防误操作的护栏而非对抗性沙箱；这里的用途是让“读清单”这一步不触碰用户数据与网络，
// 不能据此声称上游代码被隔离。运行期的上游代码与其他 Magi 扩展一样以用户权限运行（full-trust）。
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export const COMPAT_DIR = realpathSync(join(dirname(fileURLToPath(import.meta.url)), '..'));

/** 创建空临时 HOME，运行 fn(home)，结束后删除。 */
export function withTempHome(fn) {
  const home = mkdtempSync(join(tmpdir(), 'magi-record-'));
  mkdirSync(join(home, '.dsh'), { recursive: true });
  try { return fn(realpathSync(home)); } finally { rmSync(home, { recursive: true, force: true }); }
}

/**
 * @param {{ script: string, args: string[], readable: string[], home: string, label: string }} options
 * @returns {string} 子进程的 stdout
 */
export function runRestricted({ script, args, readable, home, label }) {
  const reads = [...new Set([COMPAT_DIR, realpathSync(process.execPath), home, ...readable])];
  const result = spawnSync(process.execPath, [
    '--permission', ...reads.map((path) => `--allow-fs-read=${path}`), `--allow-fs-write=${home}`, script, ...args,
  ], {
    encoding: 'utf8', timeout: 60_000, maxBuffer: 32 * 1024 * 1024, cwd: home,
    // 不继承父进程环境：凭据类环境变量不进入被记录的上游代码。
    env: { PATH: process.env.PATH ?? '', HOME: home, USERPROFILE: home, LOCALAPPDATA: home, APPDATA: home, TMPDIR: home, TEMP: home, TMP: home },
  });
  if (result.status !== 0) throw new Error(`${label}失败：${(result.stderr || result.stdout || String(result.error ?? '')).trim().slice(0, 1200)}`);
  return result.stdout;
}
