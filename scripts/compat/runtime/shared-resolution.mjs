// 共享依赖解析：发行清单声明的宿主运行库使用唯一实例；其余裸导入由原包依赖图解析。
// 不因包名带 dsh 前缀就重定向，也不重写共享包自身的嵌套依赖版本。
//
// 只做解析，不改写模块内容；找不到时沿用 Node 默认的 ERR_MODULE_NOT_FOUND，错误里带原始说明符。
import { createRequire, isBuiltin, registerHooks } from 'node:module';
import { existsSync, realpathSync } from 'node:fs';
import { dirname, join, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { fileURLToPath } from 'node:url';
import { hostDependencies, packageName } from '../core/host-dependencies.mjs';

const isBare = (specifier) => !/^(\.{0,2}\/|file:|node:|data:|https?:)/.test(specifier) && !isBuiltin(specifier);

/**
 * @param {string} sharedRoot 含 node_modules 的共享依赖目录
 * @param {Record<string, string>} [aliases] 随 Magi 发行的内部模块别名（说明符 → 绝对文件路径），如 `@magi/compat-dsh`
 */
export function installSharedResolution(sharedRoot, aliases = {}) {
  const provided = hostDependencies(sharedRoot);
  const anchor = pathToFileURL(join(sharedRoot, '__shared__.mjs')).href;
  const sharedRequire = createRequire(anchor);
  const sharedPrefix = pathToFileURL(`${join(sharedRoot, 'node_modules')}/`).href;
  const targets = new Map(Object.entries(aliases).map(([name, file]) => [name, pathToFileURL(file).href]));
  const upstreamDirectory = aliases['@magi/compat-upstream-root'] ? realpathSync(aliases['@magi/compat-upstream-root']) : undefined;
  const upstreamRoot = upstreamDirectory ? pathToFileURL(upstreamDirectory + '/').href : undefined;
  const facades = new Map([
    ['node:fs', 'profile-fs.mjs'], ['fs', 'profile-fs.mjs'],
    ['node:fs/promises', 'profile-promises.mjs'], ['fs/promises', 'profile-promises.mjs'],
    ['node:child_process', 'profile-process.mjs'], ['child_process', 'profile-process.mjs'],
  ].map(([name, file]) => [name, new URL(file, import.meta.url).href]));
  return registerHooks({
    resolve(specifier, context, nextResolve) {
      const alias = targets.get(specifier);
      if (alias) return nextResolve(alias, context);
      // 仅上游代码使用 profile 外观；兼容层自身与共享依赖继续使用原生模块，
      // 不全局替换 Node 的 fs，不影响 daemon RPC 和 Magi 自己的文件操作。
      const parent = context.parentURL;
      const fromUpstream = typeof parent === 'string' && upstreamRoot && parent.startsWith(upstreamRoot);
      // require() 的默认解析器不接受 file: URL 作为说明符。绝对路径可由
      // ESM 和 CJS 共用，并加载同一个无顶层 await 的 profile 模块实例。
      if (fromUpstream && facades.has(specifier)) return nextResolve(fileURLToPath(facades.get(specifier)), context);
      // 共享目录里的包按 Node 默认规则解析自己的依赖（可能有嵌套版本）；只有来自上游代码的裸导入才重定向。
      const fromShared = context.parentURL === anchor || (typeof context.parentURL === 'string' && context.parentURL.startsWith(sharedPrefix));
      // 主入口、子路径和 require.resolve 必须使用同一份隔离 peer。
      // 查找止于当前原包目录，不能被物理祖先目录或其他插件污染宿主。
      if (fromUpstream && packageName(specifier) === '@deepseek-ai/cordis') {
        let directory = dirname(fileURLToPath(parent));
        while (directory === upstreamDirectory || directory.startsWith(upstreamDirectory + sep)) {
          if (existsSync(join(directory, 'node_modules', '@deepseek-ai', 'cordis', 'package.json'))) return nextResolve(specifier, context);
          if (directory === upstreamDirectory) break;
          directory = dirname(directory);
        }
      }
      if (isBare(specifier) && provided.has(packageName(specifier)) && !fromShared) {
        // CJS 的 require.resolve 仍按原 require 的搜索路径解析，仅改 parentURL
        // 不会生效。先沿同一发行清单解析真实文件，保留 exports/require 条件，
        // 再以绝对路径交还 Node；没有副本或虚假的 package.json。
        if (context.conditions?.includes('require')) return nextResolve(sharedRequire.resolve(specifier), context);
        return nextResolve(specifier, { ...context, parentURL: anchor });
      }
      return nextResolve(specifier, context);
    },
  });
}
