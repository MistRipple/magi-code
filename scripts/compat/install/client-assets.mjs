// 上游浏览器端界面（package.json exports["./client"]）→ Magi 视图资源。
//
// 界面不翻译：上游的 React 界面原样交付，页面运行时（随 Magi 预构建的 dist/dsh-client-host.js）只负责
// 提供它依赖的宿主服务。本文件：
//   1. 找到上游 ./client 文件，确认它是 DSH 的懒加载 CJS 工厂格式（不是就明确失败，不猜）；
//   2. 在受限子进程里运行一次 apply，记录注册了哪些槽位，按共享位置合同分类；
//   3. 上游界面声明了、却没有任何 Magi 会渲染的槽位时失败：交付空白页比没有界面更糟。
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runRestricted, withTempHome } from './sandbox.mjs';

const SLOT_RECORDER = join(dirname(fileURLToPath(import.meta.url)), 'slot-recorder.mjs');
// 允许作者在入口前放许可证/说明注释、使用单引号或常规空格；只接受明确的
// DSH 懒加载调用形状，不执行猜测性的其他浏览器模块格式。
const LOADER_SIGNATURE = /window\.__ModuleLoader__\.load\(\s*\{\s*id\s*:\s*(['"])[^'"\n]+\1\s*,\s*factory\s*(?::|\()/s;

/** 上游 exports["./client"] 解析出的文件；没有声明则返回 null。 */
export function findClientFile(upstreamDir, pkg) {
  const target = pkg.exports?.['./client'];
  const relative = typeof target === 'string' ? target : target?.default;
  if (!relative) return null;
  const root = resolve(upstreamDir);
  const file = resolve(root, relative);
  if (!file.startsWith(root + sep)) throw new Error('上游客户端入口指向包外');
  if (!existsSync(file)) throw new Error(`上游声明了 ./client（${relative}）但文件不存在`);
  return { file, relative: relative.replace(/^\.\//, '') };
}

/** @returns {null | { path: string, mounted: object[], unmounted: object[] }} path 是上游包内的相对路径 */
export function analyzeNativeClient({ upstreamDir, pkg, sharedRoot }) {
  const client = findClientFile(upstreamDir, pkg);
  if (!client) return null;
  if (!LOADER_SIGNATURE.test(readFileSync(client.file, 'utf8'))) {
    throw new Error('上游 ./client 不是 DSH 的懒加载 CJS 工厂格式（window.__ModuleLoader__.load({ id, factory })）；翻译器不猜测其他格式');
  }
  const shared = realpathSync(sharedRoot);
  const root = realpathSync(upstreamDir);
  const slots = withTempHome((home) => {
    const resultPath = join(home, 'slots.json');
    runRestricted({ script: SLOT_RECORDER, args: [realpathSync(client.file), shared, resultPath], readable: [shared, root], home, label: '界面槽位记录' });
    return JSON.parse(readFileSync(resultPath, 'utf8'));
  });
  const mounted = slots.filter((slot) => slot.disposition === 'mounted');
  const unmounted = slots.filter((slot) => slot.disposition !== 'mounted');
  const missing = unmounted.filter(slot => slot.disposition !== 'alternate');
  if (missing.length) {
    throw new Error(`该插件需要尚未接入的界面位置：${missing.map(slot => slot.name).join('、')}。本次安装已停止，不会忽略这些界面功能后声称插件完整可用。`);
  }
  if (mounted.length === 0) {
    throw new Error(`上游界面注册了 ${slots.map((slot) => slot.name).join('、') || '（无）'}，没有任何一个是 Magi 会渲染的槽位；不交付空白设置页`);
  }
  return { path: client.relative, mounted, unmounted };
}
