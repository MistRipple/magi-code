// 上游源码的静态扫描：只读语法树，不执行任何上游代码。供各翻译器复用。
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { extname, join, relative } from 'node:path';
import ts from 'typescript';

const SOURCE_EXTENSIONS = new Set(['.ts', '.tsx', '.js', '.mjs', '.cjs', '.jsx']);
const SKIP_DIRECTORIES = new Set(['node_modules', '.git', 'lib', 'dist', 'tests', 'test', '__tests__']);

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

function bump(map, key, file) {
  const entry = map.get(key) ?? { count: 0, files: new Set() };
  entry.count += 1;
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

/**
 * @param {string} root 上游包根目录
 * @param {{ contextNames?: RegExp, directories?: string[], chainDepth?: number }} options
 */
export function scanSources(root, options = {}) {
  const contextNames = options.contextNames ?? /^(ctx|context|\w+Ctx)$/;
  const depth = options.chainDepth ?? 3;
  const files = listSourceFiles(root, options.directories);
  const imports = new Map();
  const calls = new Map();
  const injects = new Map();
  let lines = 0;
  for (const file of files) {
    const text = readFileSync(file, 'utf8');
    lines += text.split('\n').length;
    const rel = relative(root, file);
    const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, extname(file).endsWith('x') ? ts.ScriptKind.TSX : undefined);
    const visit = (node) => {
      if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)) {
        const typeOnly = Boolean(node.importClause?.isTypeOnly) || !node.importClause;
        bump(imports, `${typeOnly ? 'type:' : ''}${node.moduleSpecifier.text}`, rel);
      } else if (ts.isExportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
        bump(imports, node.moduleSpecifier.text, rel);
      } else if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword && ts.isStringLiteralLike(node.arguments[0])) {
        bump(imports, node.arguments[0].text, rel);
      } else if (ts.isPropertyAccessExpression(node) && !ts.isPropertyAccessExpression(node.parent)) {
        const chain = chainOf(node);
        if (chain && contextNames.test(chain[0]) && chain.length > 1) {
          bump(calls, ['ctx', ...chain.slice(1, depth)].join('.'), rel);
        }
      }
      // `ctx.inject(['a', 'b'], …)` 与 `export const inject = [...]`
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === 'inject') {
        const first = node.arguments[0];
        if (first && ts.isArrayLiteralExpression(first)) {
          for (const element of first.elements) if (ts.isStringLiteralLike(element)) bump(injects, element.text, rel);
        }
      }
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === 'inject'
        && node.initializer && ts.isArrayLiteralExpression(node.initializer)) {
        for (const element of node.initializer.elements) if (ts.isStringLiteralLike(element)) bump(injects, element.text, rel);
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  const plain = (map) => Object.fromEntries([...map].sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => [key, { count: value.count, files: [...value.files].sort() }]));
  return { fileCount: files.length, lineCount: lines, imports: plain(imports), calls: plain(calls), injects: plain(injects) };
}
