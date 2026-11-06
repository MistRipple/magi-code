// 共享依赖解析：上游原包不自带依赖（cordis、dsh-llm、pi-ai、react 等都是 peerDependencies），
// 因此所有裸模块说明符统一解析到 Magi 随发行的共享依赖目录。上游只会拿到这一份运行时，
// 不会出现两份 Cordis 或 Schemastery 导致 instanceof 失效。
//
// 只做解析，不改写模块内容；找不到时沿用 Node 默认的 ERR_MODULE_NOT_FOUND，错误里带原始说明符。
import { readFileSync } from 'node:fs';
import { isBuiltin, registerHooks } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const isBare = (specifier) => !/^(\.{0,2}\/|file:|node:|data:|https?:)/.test(specifier) && !isBuiltin(specifier);

/**
 * @param {string} sharedRoot 含 node_modules 的共享依赖目录
 * @param {Record<string, string>} [aliases] 随 Magi 发行的内部模块别名（说明符 → 绝对文件路径），如 `@magi/compat-dsh`
 */
export function installSharedResolution(sharedRoot, aliases = {}) {
  const provided = new Set(Object.keys(JSON.parse(readFileSync(join(sharedRoot, 'package.json'), 'utf8')).dependencies).filter((name) => name !== '@deepseek-ai/schemastery' && name !== 'semver' && name !== 'typescript'));
  const anchor = pathToFileURL(join(sharedRoot, '__shared__.mjs')).href;
  const sharedPrefix = pathToFileURL(`${join(sharedRoot, 'node_modules')}/`).href;
  const targets = new Map(Object.entries(aliases).map(([name, file]) => [name, pathToFileURL(file).href]));
  return registerHooks({
    resolve(specifier, context, nextResolve) {
      const alias = targets.get(specifier);
      if (alias) return nextResolve(alias, context);
      // 共享目录里的包按 Node 默认规则解析自己的依赖（可能有嵌套版本）；只有来自上游代码的裸导入才重定向。
      const fromShared = typeof context.parentURL === 'string' && context.parentURL.startsWith(sharedPrefix);
      const packageName = specifier.startsWith('@') ? specifier.split('/').slice(0, 2).join('/') : specifier.split('/')[0];
      return isBare(specifier) && (provided.has(packageName) || packageName.startsWith('@deepseek-ai/dsh-')) && !fromShared ? nextResolve(specifier, { ...context, parentURL: anchor }) : nextResolve(specifier, context);
    },
  });
}
