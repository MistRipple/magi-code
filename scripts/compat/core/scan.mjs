// 上游源码的静态扫描：只读语法树，不执行任何上游代码。供各翻译器复用。
//
// 判定“某个标识符是不是宿主给的上下文对象”靠数据流，不靠变量名：从生态给出的种子函数（DSH 的 apply、
// pi 的默认导出工厂）出发，沿回调与同文件调用传播。LLM 请求里的 `context.messages` 之类同名变量不会混进来。
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, extname, join, relative, resolve } from 'node:path';
import ts from 'typescript';
import { conditionalService, guardedRegistration } from './optional.mjs';

const SOURCE_EXTENSIONS = new Set(['.ts', '.tsx', '.js', '.mjs', '.cjs', '.jsx']);
const SKIP_DIRECTORIES = new Set(['node_modules', '.git', 'lib', 'dist', 'tests', 'test', '__tests__']);
const RESOLVE_SUFFIXES = ['', '.ts', '.tsx', '.js', '.mjs', '.cjs', '/index.ts', '/index.tsx', '/index.js', '/index.mjs'];

export function listSourceFiles(root, directories = ['src']) {
  const files = [];
  const walk = (directory) => {
    let entries;
    try { entries = readdirSync(directory); } catch { return; }
    for (const name of entries) {
      const full = join(directory, name);
      const stat = statSync(full);
      if (stat.isDirectory()) {
        if (!SKIP_DIRECTORIES.has(name)) walk(full);
      } else if (SOURCE_EXTENSIONS.has(extname(name)) && !name.endsWith('.d.ts') && !/\.(test|spec)\./.test(name)) {
        files.push(full);
      }
    }
  };
  for (const directory of directories) walk(join(root, directory));
  return files.sort();
}

/** 相对导入的实际文件：按 TS/JS 常见规则尝试扩展名与 index；`./x.js` 可指向 `x.ts`。找不到就返回 undefined。 */
function resolveRelative(fromFile, specifier) {
  const base = resolve(dirname(fromFile), specifier);
  const candidates = [base];
  if (/\.(m?js|cjs)$/.test(base)) candidates.push(base.replace(/\.(m?js|cjs)$/, '.ts'), base.replace(/\.(m?js|cjs)$/, '.tsx'));
  for (const candidate of candidates) {
    for (const suffix of RESOLVE_SUFFIXES) {
      const file = candidate + suffix;
      if (existsSync(file) && statSync(file).isFile()) return file;
    }
  }
  return undefined;
}

/**
 * 从入口文件出发，沿相对导入收集会随入口一起运行的文件。
 * 相对导入解析不到时抛错：漏扫一个文件就可能漏掉一个不可翻译的 API，不能静默跳过。
 */
export function reachableFiles(entries) {
  const seen = new Set();
  const queue = (Array.isArray(entries) ? entries : [entries]).map((entry) => resolve(entry));
  while (queue.length > 0) {
    const file = queue.pop();
    if (seen.has(file)) continue;
    if (!existsSync(file)) throw new Error(`入口依赖的文件不存在：${file}`);
    seen.add(file);
    const source = ts.createSourceFile(file, readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true);
    const follow = (specifier) => {
      if (!specifier.startsWith('.')) return;
      const target = resolveRelative(file, specifier);
      if (!target) throw new Error(`无法解析相对导入 ${specifier}（来自 ${file}）`);
      if (SOURCE_EXTENSIONS.has(extname(target))) queue.push(target);
    };
    const visit = (node) => {
      if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) follow(node.moduleSpecifier.text);
      else if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword && ts.isStringLiteralLike(node.arguments[0])) follow(node.arguments[0].text);
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  return [...seen].sort();
}

function bump(map, key, file, optional = false) {
  const entry = map.get(key) ?? { count: 0, optionalCount: 0, files: new Set() };
  entry.count += 1;
  if (optional) entry.optionalCount += 1;
  entry.files.add(file);
  map.set(key, entry);
}

function chainOf(node) {
  const parts = [];
  let current = node;
  while (ts.isPropertyAccessExpression(current) || ts.isElementAccessExpression(current)) {
    if (ts.isElementAccessExpression(current)) {
      if (!ts.isStringLiteralLike(current.argumentExpression)) return null;
      parts.unshift(current.argumentExpression.text);
    } else parts.unshift(current.name.text);
    current = current.expression;
  }
  if (!ts.isIdentifier(current)) return null;
  parts.unshift(current.text);
  return parts;
}

const IDENT = (node) => (node && ts.isIdentifier(node) ? node.text : undefined);
const isFunctionLike = (node) => node && (ts.isArrowFunction(node) || ts.isFunctionExpression(node) || ts.isFunctionDeclaration(node) || ts.isMethodDeclaration(node));

/** 形参名；解构形参没有单一名字，返回 undefined（不猜）。 */
function paramName(fn, index = 0) {
  const parameter = fn.parameters?.[index];
  return parameter ? IDENT(parameter.name) : undefined;
}

/** 对象字面量里值为函数的属性（含方法简写）。 */
function functionProperties(objectLiteral, names) {
  const found = [];
  for (const property of objectLiteral.properties ?? []) {
    const name = property.name && (IDENT(property.name) ?? (ts.isStringLiteralLike(property.name) ? property.name.text : undefined));
    if (!name || !names.includes(name)) continue;
    if (ts.isMethodDeclaration(property)) found.push({ name, fn: property });
    else if (ts.isPropertyAssignment(property) && isFunctionLike(property.initializer)) found.push({ name, fn: property.initializer });
  }
  return found;
}

/**
 * 生态数据流描述：
 *   seeds(source) → [{ fn, index, label }]   种子函数及其上下文形参
 *   derive(call, label) → [{ fn, index, label }]   由已知上下文的调用派生出的回调及其上下文形参
 *   keyOf(label) → 报告里的前缀（如 ctx / pi）
 *   eventLabel   事件监听所属的上下文标签
 */
export const DSH_FLOW = {
  keyOf: () => 'ctx',
  eventLabel: 'ctx',
  seeds(source) {
    const seeds = [];
    ts.forEachChild(source, (node) => {
      if (ts.isFunctionDeclaration(node) && node.name?.text === 'apply') seeds.push({ fn: node, index: 0, label: 'ctx' });
      if (ts.isVariableStatement(node)) {
        for (const d of node.declarationList.declarations) {
          if (IDENT(d.name) === 'apply' && isFunctionLike(d.initializer)) seeds.push({ fn: d.initializer, index: 0, label: 'ctx' });
        }
      }
    });
    return seeds;
  },
  derive(call, label) {
    if (!ts.isPropertyAccessExpression(call.expression)) return [];
    if (!['inject', 'plugin', 'effect', 'collect', 'fiber', 'scope', 'isolate', 'intercept'].includes(call.expression.name.text)) return [];
    return call.arguments.filter(isFunctionLike).map((fn) => ({ fn, index: 0, label }));
  },
};

export const PI_FLOW = {
  keyOf: (label) => (label === 'ctx' ? 'ctx' : 'pi'),
  eventLabel: 'pi',
  seeds(source) {
    const seeds = [];
    const named = new Map();
    ts.forEachChild(source, (node) => {
      if (ts.isFunctionDeclaration(node) && node.name) named.set(node.name.text, node);
      if (ts.isVariableStatement(node)) for (const d of node.declarationList.declarations) {
        if (IDENT(d.name) && isFunctionLike(d.initializer)) named.set(d.name.text, d.initializer);
      }
    });
    ts.forEachChild(source, (node) => {
      const isDefault = ts.isFunctionDeclaration(node) && node.modifiers?.some((m) => m.kind === ts.SyntaxKind.DefaultKeyword);
      if (isDefault) seeds.push({ fn: node, index: 0, label: 'pi' });
      if (ts.isExportAssignment(node) && !node.isExportEquals) {
        if (isFunctionLike(node.expression)) seeds.push({ fn: node.expression, index: 0, label: 'pi' });
        else if (IDENT(node.expression) && named.has(node.expression.text)) seeds.push({ fn: named.get(node.expression.text), index: 0, label: 'pi' });
      }
    });
    return seeds;
  },
  derive(call, label) {
    if (label !== 'pi' || !ts.isPropertyAccessExpression(call.expression)) return [];
    const method = call.expression.name.text;
    const out = [];
    // pi.on(event, (event, ctx) => …)
    if (method === 'on') for (const arg of call.arguments.slice(1)) if (isFunctionLike(arg)) out.push({ fn: arg, index: 1, label: 'ctx' });
    // pi.registerCommand(name, { handler(args, ctx) }) / pi.registerTool({ execute(id, params, signal, onUpdate, ctx) })
    if (method === 'registerCommand' || method === 'registerTool') {
      for (const arg of call.arguments) {
        if (!ts.isObjectLiteralExpression(arg)) continue;
        for (const { name, fn } of functionProperties(arg, ['handler', 'execute'])) {
          out.push({ fn, index: name === 'execute' ? 4 : 1, label: 'ctx' });
        }
      }
    }
    return out;
  },
};

/**
 * 跨文件上下文传播。输入是整个文件集合：
 *  - 每个文件解析一次；具名函数（含 `export function`/`export const f = () =>`）按“文件 + 名字”登记；
 *  - 上下文被当作实参传给函数时，函数可能在同文件，也可能经 `import { f } from './x'` 来自别的文件，
 *    后者通过导入表解析到目标文件里的同名导出函数；
 *  - 迭代到不动点。传播不到的调用（例如经对象方法或动态调用）不会被识别，
 *    因此扫描器同时统计“把上下文传给了无法解析的目标”的次数，作为 unresolvedContextFlows 返回，由报告列为 unknown。
 * 返回 Map<文件, { source, scopes: Map<fn, Map<name,label>> }> 与未解析次数。
 */
function analyzeContexts(files, flow) {
  const parsed = new Map();
  for (const file of files) {
    const text = readFileSync(file, 'utf8');
    parsed.set(file, ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, extname(file).endsWith('x') ? ts.ScriptKind.TSX : undefined));
  }
  const info = new Map();
  for (const [file, source] of parsed) {
    const functions = new Map();
    const collect = (node) => {
      if (ts.isFunctionDeclaration(node) && node.name) functions.set(node.name.text, node);
      if (ts.isVariableDeclaration(node) && IDENT(node.name) && isFunctionLike(node.initializer)) functions.set(node.name.text, node.initializer);
      ts.forEachChild(node, collect);
    };
    collect(source);
    // 导入表：本地名 → { file, exported }
    const imports = new Map();
    for (const statement of source.statements) {
      if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier) || !statement.moduleSpecifier.text.startsWith('.')) continue;
      const target = resolveRelative(file, statement.moduleSpecifier.text);
      if (!target || !parsed.has(target)) continue;
      const named = statement.importClause?.namedBindings;
      if (named && ts.isNamedImports(named)) {
        for (const element of named.elements) imports.set(element.name.text, { file: target, exported: (element.propertyName ?? element.name).text });
      }
      const defaultName = IDENT(statement.importClause?.name);
      if (defaultName) imports.set(defaultName, { file: target, exported: 'default' });
    }
    info.set(file, { source, functions, imports, scopes: new Map() });
  }
  const add = (file, fn, name, label) => {
    if (!name) return false;
    const scopes = info.get(file).scopes;
    const names = scopes.get(fn) ?? new Map();
    if (names.get(name) === label) return false;
    names.set(name, label); scopes.set(fn, names);
    return true;
  };
  for (const [file, { source }] of info) for (const seed of flow.seeds(source)) add(file, seed.fn, paramName(seed.fn, seed.index), seed.label);
  const labelAt = (file, name, at) => {
    const scopes = info.get(file).scopes;
    for (let n = at.parent; n; n = n.parent) { const label = scopes.get(n)?.get(name); if (label) return label; }
    return undefined;
  };
  /** 目标函数：同文件具名函数，或导入来的具名/默认导出函数。 */
  const targetOf = (file, calleeName) => {
    const own = info.get(file).functions.get(calleeName);
    if (own) return { file, fn: own };
    const imported = info.get(file).imports.get(calleeName);
    if (!imported) return undefined;
    const target = info.get(imported.file);
    if (imported.exported === 'default') {
      for (const statement of target.source.statements) {
        if (ts.isFunctionDeclaration(statement) && statement.modifiers?.some((m) => m.kind === ts.SyntaxKind.DefaultKeyword)) return { file: imported.file, fn: statement };
        if (ts.isExportAssignment(statement) && isFunctionLike(statement.expression)) return { file: imported.file, fn: statement.expression };
        if (ts.isExportAssignment(statement) && IDENT(statement.expression) && target.functions.has(statement.expression.text)) return { file: imported.file, fn: target.functions.get(statement.expression.text) };
      }
      return undefined;
    }
    const fn = target.functions.get(imported.exported);
    return fn ? { file: imported.file, fn } : undefined;
  };
  let unresolved = 0;
  let changed = true;
  let pass = 0;
  while (changed) {
    changed = false;
    unresolved = 0; // 每轮重新统计，只保留不动点上的结果
    if (++pass > 50) throw new Error('上下文传播未收敛');
    for (const [file, { source }] of info) {
      const walk = (node) => {
        if (ts.isCallExpression(node)) {
          const root = ts.isPropertyAccessExpression(node.expression) ? IDENT(rootOf(node.expression)) : undefined;
          const label = root && labelAt(file, root, node);
          if (label) for (const d of flow.derive(node, label)) changed = add(file, d.fn, paramName(d.fn, d.index), d.label) || changed;
          const callee = IDENT(node.expression);
          const contextArgs = node.arguments.map((arg, i) => ({ i, label: IDENT(arg) && labelAt(file, IDENT(arg), node) })).filter((x) => x.label);
          if (callee && contextArgs.length > 0) {
            const target = targetOf(file, callee);
            if (target) for (const { i, label: argLabel } of contextArgs) changed = add(target.file, target.fn, paramName(target.fn, i), argLabel) || changed;
            // 调用的是本文件/相对导入里解析不到的函数：可能是第三方库（不会反向调用上下文），也可能漏扫，
            // 这里不计入；只有“上下文被赋给对象属性/数组/闭包外变量”这类逃逸才算未解析，见下。
          }
          // 上下文逃逸：作为对象方法的实参（`obj.fn(ctx)`）无法静态解析目标。
          if (ts.isPropertyAccessExpression(node.expression) && !label) {
            for (const arg of node.arguments) if (IDENT(arg) && labelAt(file, IDENT(arg), node)) unresolved += 1;
          }
        }
        // 上下文被存进变量/属性：`this.ctx = ctx`、`const holder = { ctx }`、`state.ctx = ctx`
        if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken && IDENT(node.right) && labelAt(file, IDENT(node.right), node) && !IDENT(node.left)) unresolved += 1;
        if (ts.isShorthandPropertyAssignment(node) && labelAt(file, node.name.text, node)) unresolved += 1;
        if (ts.isPropertyAssignment(node) && IDENT(node.initializer) && labelAt(file, IDENT(node.initializer), node)) unresolved += 1;
        ts.forEachChild(node, walk);
      };
      walk(source);
    }
  }
  return { info, unresolved };
}

function rootOf(expr) {
  let current = expr;
  while (ts.isPropertyAccessExpression(current) || ts.isElementAccessExpression(current)) current = current.expression;
  return current;
}

/**
 * @param {string} root 上游包根目录
 * @param {{ directories?: string[], entry?: string|string[], files?: string[], flow?: object, chainDepth?: number }} options
 * entry（沿相对导入收集，已构建产物或入口清单）、files（调用方已确定的文件集合）、directories（按目录扫描源码）三选一。
 */
export function scanSources(root, options = {}) {
  const flow = options.flow ?? DSH_FLOW;
  const depth = options.chainDepth ?? 3;
  const files = options.files ?? (options.entry ? reachableFiles(options.entry) : listSourceFiles(root, options.directories));
  const imports = new Map();
  const symbols = new Map();
  const calls = new Map();
  const injects = new Map();
  const serviceReads = new Map();
  const serviceMembers = new Map();
  const events = new Map();
  const profileMutations = [];
  const profilePackageProcesses = [];
  let lines = 0;
  const { info, unresolved } = analyzeContexts(files, flow);
  for (const file of files) {
    const text = readFileSync(file, 'utf8');
    lines += text.split('\n').length;
    const rel = relative(root, file);
    const { source, scopes } = info.get(file);
    // 仅将明确构造 profile patch 路径且调用 fs 写入的模块作为生命周期准入证据；
    // 普通业务文件写入（截图、缓存等）不因此获得插件管理授权。
    const fsWriters = new Set(), fsNamespaces = new Set();
    const writeMethod = /^(?:writeFile|rename|unlink|rm|copyFile)(?:Sync)?$/;
    for (const statement of source.statements) if (ts.isImportDeclaration(statement)
      && /^(?:node:)?fs(?:\/promises)?$/.test(statement.moduleSpecifier?.text ?? '')) {
      const named = statement.importClause?.namedBindings;
      if (statement.importClause?.name) fsNamespaces.add(statement.importClause.name.text);
      if (named && ts.isNamespaceImport(named)) fsNamespaces.add(named.name.text);
      if (named && ts.isNamedImports(named)) for (const binding of named.elements) {
        if (writeMethod.test((binding.propertyName ?? binding.name).text)) fsWriters.add(binding.name.text);
      }
    }
    let writesProfile = false, profilePatch = false;
    const inspectProfile = node => {
      if (ts.isCallExpression(node)) {
        const callee = node.expression;
        if (ts.isIdentifier(callee) && fsWriters.has(callee.text)
          || ts.isPropertyAccessExpression(callee) && ts.isIdentifier(callee.expression) && fsNamespaces.has(callee.expression.text) && writeMethod.test(callee.name.text)) writesProfile = true;
        if (node.arguments.some(arg => ts.isStringLiteralLike(arg) && arg.text === 'cordis.patch.yml')) profilePatch = true;
      }
      ts.forEachChild(node, inspectProfile);
    };
    inspectProfile(source);
    if (writesProfile && profilePatch) profileMutations.push(rel);
    // 包管理可能仅在用户点击页面后启动。明确携带 npm/pnpm 命令并导入
    // spawn 的模块提前声明安装委托；运行时只接管当前 profile 的包操作，
    // 普通截图子进程不因此获得插件管理权限。
    const processSpawners = new Set(), processNamespaces = new Set();
    for (const statement of source.statements) if (ts.isImportDeclaration(statement)
      && /^(?:node:)?child_process$/.test(statement.moduleSpecifier?.text ?? '')) {
      const named = statement.importClause?.namedBindings;
      if (statement.importClause?.name) processNamespaces.add(statement.importClause.name.text);
      if (named && ts.isNamespaceImport(named)) processNamespaces.add(named.name.text);
      if (named && ts.isNamedImports(named)) for (const binding of named.elements) {
        if ((binding.propertyName ?? binding.name).text === 'spawn') processSpawners.add(binding.name.text);
      }
    }
    let spawnsProcess = false, namesPackageManager = false;
    const inspectProcess = node => {
      if (ts.isStringLiteralLike(node)
        && /(?:^|[/\\])(?:(?:npm|pnpm)(?:\.cmd)?|npm-cli\.js|pnpm\.(?:c?js|mjs))$/i.test(node.text)) namesPackageManager = true;
      if (ts.isCallExpression(node)) {
        const callee = node.expression;
        if (ts.isIdentifier(callee) && processSpawners.has(callee.text)
          || ts.isPropertyAccessExpression(callee) && ts.isIdentifier(callee.expression) && processNamespaces.has(callee.expression.text) && callee.name.text === 'spawn') spawnsProcess = true;
      }
      ts.forEachChild(node, inspectProcess);
    };
    inspectProcess(source);
    if (spawnsProcess && namesPackageManager) profilePackageProcesses.push(rel);
    const labelOf = (name, at) => {
      for (let n = at.parent; n; n = n.parent) { const label = scopes.get(n)?.get(name); if (label) return label; }
      return undefined;
    };
    const isDshContext = (node, at) => flow === DSH_FLOW && ts.isIdentifier(node) && labelOf(node.text, at) === 'ctx';
    // ctx.get() 的结果属于具体服务，不能因只读取 profile 名称就申请安装委托。
    const aliases = new Map();
    const ownerOf = node => { for (let parent = node.parent; parent; parent = parent.parent) if (isFunctionLike(parent)) return parent; return source; };
    const collectServices = node => {
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer) {
        const names = new Set();
        const find = value => {
          if (ts.isCallExpression(value) && ts.isPropertyAccessExpression(value.expression) && value.expression.name.text === 'get'
            && isDshContext(value.expression.expression, value) && value.arguments[0] && ts.isStringLiteralLike(value.arguments[0])) names.add(value.arguments[0].text);
          ts.forEachChild(value, find);
        };
        find(node.initializer);
        if (names.size === 1) {
          const owner = ownerOf(node), scope = aliases.get(owner) ?? new Map();
          scope.set(node.name.text, [...names][0]); aliases.set(owner, scope);
        }
      }
      ts.forEachChild(node, collectServices);
    };
    collectServices(source);
    const visit = (node) => {
      if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)) {
        const typeOnly = Boolean(node.importClause?.isTypeOnly);
        const module = node.moduleSpecifier.text;
        bump(imports, `${typeOnly ? 'type:' : ''}${module}`, rel);
        const named = node.importClause?.namedBindings;
        if (!typeOnly && named && ts.isNamedImports(named)) {
          for (const element of named.elements) if (!element.isTypeOnly) bump(symbols, `${module}#${(element.propertyName ?? element.name).text}`, rel);
        }
      } else if (ts.isExportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
        bump(imports, node.moduleSpecifier.text, rel);
      } else if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword && ts.isStringLiteralLike(node.arguments[0])) {
        bump(imports, node.arguments[0].text, rel);
      } else if (ts.isPropertyAccessExpression(node) && !ts.isPropertyAccessExpression(node.parent)) {
        const chain = chainOf(node);
        const label = chain && chain.length > 1 ? labelOf(chain[0], node) : undefined;
        if (label) bump(calls, [flow.keyOf(label), ...chain.slice(1, depth)].join('.'), rel,
          flow === DSH_FLOW && conditionalService(node, chain[1], isDshContext));
        if (chain?.length > 1) for (let parent = node.parent; parent; parent = parent.parent) {
          const service = aliases.get(parent)?.get(chain[0]);
          if (service) { bump(serviceMembers, [service, ...chain.slice(1, depth)].join('.'), rel); break; }
        }
      }
      // 事件监听：`ctx.on('tools/pre-execute', …)` / `pi.on('tool_call', …)`。事件名不是字面量记为 <dynamic>。
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && ['on', 'once'].includes(node.expression.name.text)) {
        const owner = chainOf(node.expression.expression);
        if (owner && owner.length === 1 && labelOf(owner[0], node) === flow.eventLabel) {
          const first = node.arguments[0];
          bump(events, first && ts.isStringLiteralLike(first) ? first.text : '<dynamic>', rel,
            flow === DSH_FLOW && guardedRegistration(node, source));
        }
      }
      // `ctx.inject(['a', 'b'], …)` 与 `export const inject = [...]`
      // get() 本身允许服务缺席；另记读取身份供安装授权声明使用，不把 get()
      // 误当成必需 inject，也不能漏掉经辅助函数取得的 pluginManager。
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === 'get'
        && isDshContext(node.expression.expression, node) && node.arguments[0] && ts.isStringLiteralLike(node.arguments[0])) {
        bump(serviceReads, node.arguments[0].text, rel);
      }
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === 'inject') {
        const first = node.arguments[0];
        if (first && ts.isArrayLiteralExpression(first)) for (const element of first.elements) if (ts.isStringLiteralLike(element)) {
          bump(injects, element.text, rel, isDshContext(node.expression.expression, node));
        }
      }
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === 'inject' && node.initializer && ts.isArrayLiteralExpression(node.initializer)) {
        for (const element of node.initializer.elements) if (ts.isStringLiteralLike(element)) bump(injects, element.text, rel);
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  const plain = (map) => Object.fromEntries([...map].sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => [key, { count: value.count, optional: value.optionalCount === value.count, files: [...value.files].sort() }]));
  return { fileCount: files.length, lineCount: lines, files: files.map((file) => relative(root, file)), imports: plain(imports), symbols: plain(symbols), calls: plain(calls), injects: plain(injects), serviceReads: plain(serviceReads), serviceMembers: plain(serviceMembers), profileMutations, profilePackageProcesses, events: plain(events), unresolvedContextFlows: unresolved };
}
