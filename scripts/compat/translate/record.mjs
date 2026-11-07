// 受限记录：在 Node 权限模型的子进程里加载上游原入口并运行一次 apply，记录它注册了什么。
import { realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync, writeFileSync } from 'node:fs';
import { runRestricted, withTempHome } from './sandbox.mjs';
import { allSwitchesOn } from './variants.mjs';

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
    const run = (overrides) => {
      const overridesPath = join(home, 'overrides.json');
      writeFileSync(overridesPath, JSON.stringify(overrides));
      const resultPath = join(home, 'result.json');
      runRestricted({ script: RECORDER, args: [entry, overridesPath, sharedRoot, resultPath, upstreamDir], readable: [sharedRoot, upstreamDir], home, label: '受限记录' });
      return JSON.parse(readFileSync(resultPath, 'utf8'));
    };
    const first = run({});
    const variants = [{ overrides: {}, recorded: first }];
    const on = allSwitchesOn(first.configSchema);
    if (on) variants.push({ overrides: on, recorded: run(on) });

    const merged = { tools: new Map(), commands: new Map(), providers: new Map(), routes: new Map(), listeners: new Set(), configSchema: first.configSchema, name: first.name, inject: first.inject, variants: [] };
    for (const { overrides, recorded } of variants) {
      for (const tool of recorded.tools) merged.tools.set(tool.id, tool);
      for (const command of recorded.commands) merged.commands.set(command.id, command);
      for (const provider of recorded.providers) merged.providers.set(provider.id, provider);
      for (const route of recorded.routes) merged.routes.set(`${route.kind}:${route.path}`, route);
      for (const name of recorded.listeners) merged.listeners.add(name);
      merged.variants.push({ overrides, tools: recorded.tools.map((tool) => tool.id), commands: recorded.commands.map((command) => command.id) });
    }
    return { ...merged, tools: [...merged.tools.values()], commands: [...merged.commands.values()], providers: [...merged.providers.values()], routes: [...merged.routes.values()], listeners: [...merged.listeners].sort() };
  });
}
