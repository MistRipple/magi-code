// 从 Magi 源码读取“钩子目录”与“宿主 RPC 方法”两类事实，用来核对 capabilities.mjs。
// 读取的是源码文本（不执行 Rust）；解析失败时抛错，而不是返回空集合——空集合会让核对静默通过。
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const read = (relative) => readFileSync(resolve(ROOT, relative), 'utf8');

const MODE = { OBSERVE: 'observe', ADVISE: 'advise', GUARD: 'guard' };
const GROUPS = {
  GUARD: ['guard'], OBSERVE: ['observe'], GUARD_ADVISE: ['guard', 'advise'], ADVISE: ['advise'], OBSERVE_ADVISE: ['observe', 'advise'],
};

/** @returns {Map<string, { modes: string[], wired: string[], replace: boolean }>} */
export function readHookCatalog() {
  const text = read('crates/magi-extension-host/src/protocol.rs');
  const start = text.indexOf('pub const HOOKS');
  if (start === -1) throw new Error('protocol.rs 中找不到 HOOKS 目录');
  const end = text.indexOf('];', start);
  const body = text.slice(start, end);
  const catalog = new Map();
  for (const block of body.split('HookSpec {').slice(1)) {
    const name = /name:\s*"([^"]+)"/.exec(block)?.[1];
    const modes = /modes:\s*([A-Z_]+)/.exec(block)?.[1];
    const wired = /wired:\s*([A-Z_]+)/.exec(block)?.[1];
    const replace = /replace:\s*(true|false)/.exec(block)?.[1];
    if (!name || !GROUPS[modes] || !GROUPS[wired] || !replace) throw new Error(`无法解析钩子条目：${block.slice(0, 80)}`);
    catalog.set(name, { modes: GROUPS[modes], wired: GROUPS[wired], replace: replace === 'true' });
  }
  if (catalog.size < 10) throw new Error(`钩子目录解析异常：只得到 ${catalog.size} 项`);
  return catalog;
}

/**
 * 宿主 → daemon 的 RPC 方法名。两处事实源：
 * - instance.rs：协议层直接处理的注册/日志方法（register.*、unregister.*、host.*）；
 * - extension_services.rs：daemon 业务服务分发（model.invoke、settings.*、modelSource.* 等）的 match 分支。
 */
export function readHostMethods() {
  const methods = new Set();
  const collect = (text) => {
    for (const m of text.matchAll(/"([a-zA-Z]+(?:\.[a-zA-Z]+)+)"/g)) methods.add(m[1]);
  };
  const instance = read('crates/magi-extension-host/src/instance.rs');
  for (const m of instance.matchAll(/^\s*(?:\|\s*)?"((?:register|unregister|host)\.[a-z]+)"/gm)) methods.add(m[1]);
  // 服务分发：只取 match 分支（"a" | "b" =>），避免把测试里的字符串当成方法。
  const services = read('crates/magi-daemon/src/daemon/extension_services.rs');
  for (const arm of services.matchAll(/^\s*((?:"[a-zA-Z]+(?:\.[a-zA-Z]+)+"\s*\|?\s*)+)=>/gm)) collect(arm[1]);
  // 需要活动调用租约的业务服务：instance.rs 里 `matches!(method, "model.invoke" | …)` 的列表
  //（钩子内同样以 ReentryDenied 拒绝，见 readHookReentryDenied）。
  for (const method of readHookReentryDenied()) methods.add(method);
  if (methods.size < 12) throw new Error(`宿主方法解析异常：只得到 ${methods.size} 项`);
  return methods;
}
/** 钩子处理器里调用就会被 ReentryDenied 拒绝的反向服务（instance.rs）。 */
export function readHookReentryDenied() {
  const text = read('crates/magi-extension-host/src/instance.rs');
  const at = text.indexOf('ErrorKind::ReentryDenied');
  if (at === -1) throw new Error('instance.rs 中找不到 ReentryDenied 分支');
  const before = text.slice(0, at);
  const start = before.lastIndexOf('if matches!(');
  if (start === -1) throw new Error('instance.rs 中找不到 ReentryDenied 方法列表');
  const list = [...before.slice(start).matchAll(/"([a-zA-Z]+(?:\.[a-zA-Z]+)+)"/g)].map((m) => m[1]);
  if (!list.includes('model.invoke') || !list.includes('ask.user')) throw new Error('ReentryDenied 列表解析异常');
  return new Set(list);
}
export { MODE };
