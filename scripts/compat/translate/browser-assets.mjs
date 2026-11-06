import { readFileSync, copyFileSync } from 'node:fs';
import { join } from 'node:path';
import ts from 'typescript';
import { browserLibraries } from '../runtime/client/modules.mjs';

export function copyBrowserAssets(clientFile, sharedRoot, destination) {
  const source = ts.createSourceFile(clientFile, readFileSync(clientFile, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const names = new Set();
  const visit = (node) => {
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'require') {
      if (!node.arguments[0] || !ts.isStringLiteralLike(node.arguments[0])) throw new Error('浏览器模块导入必须使用可核对的字面量名称');
      names.add(node.arguments[0].text);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  const manifest = JSON.parse(readFileSync(join(sharedRoot, 'dist/browser-modules.json'), 'utf8'));
  const files = browserLibraries(names).flatMap((library) => {
    if (!manifest[library.name]) throw new Error(`发行包缺少浏览器模块 ${library.name}`);
    return manifest[library.name];
  });
  for (const file of files) {
    if (!/^[a-z0-9-]+\.(js|css)$/.test(file)) throw new Error('浏览器运行库清单路径无效');
    copyFileSync(join(sharedRoot, 'dist', file), join(destination, file));
  }
  return { scripts: files.filter((file) => file.endsWith('.js')), styles: files.filter((file) => file.endsWith('.css')) };
}
