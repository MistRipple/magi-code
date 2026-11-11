// 受限记录：在 Node 权限模型的子进程里加载上游原入口并运行一次 apply，记录它注册了什么。
import { realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { runRestricted, withTempHome } from './sandbox.mjs';

const RECORDER = join(dirname(fileURLToPath(import.meta.url)), 'recorder.mjs');

/**
 * @param {{ entry: string, sharedRoot: string, upstreamDir: string }} options entry 是上游入口（ESM）的绝对路径
 */
export function recordUpstream(options) {
  const sharedRoot = realpathSync(options.sharedRoot);
  const upstreamDir = realpathSync(options.upstreamDir);
  // 权限白名单按真实路径匹配：入口也必须取真实路径，否则位于符号链接目录（如 macOS 的 /tmp、/var）下的包会被拒绝读取。
  const entry = realpathSync(options.entry);
  return withTempHome((home) => {
    const resultPath = join(home, 'result.json');
    runRestricted({ script: RECORDER, args: [entry, sharedRoot, resultPath, upstreamDir, options.readSessionDirectory ? '1' : '0', JSON.stringify(options.nullableEvents ?? []), options.webProviders ? '1' : '0', options.settingsNamespace ?? ''], readable: [sharedRoot, upstreamDir], home, label: '受限记录' });
    return JSON.parse(readFileSync(resultPath, 'utf8'));
  });
}
