import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import ts from 'typescript';

const sourcePath = new URL('../src/lib/file-reference.ts', import.meta.url);
const source = await readFile(sourcePath, 'utf8');
const transpiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ESNext,
    target: ts.ScriptTarget.ES2022,
  },
  fileName: pathToFileURL(sourcePath.pathname).href,
}).outputText;
const moduleUrl = `data:text/javascript;base64,${Buffer.from(transpiled).toString('base64')}`;
const fileReference = await import(moduleUrl);

assert.equal(
  fileReference.normalizeImplicitFileReferenceTarget('SKILL.md'),
  null,
  '裸文件名没有确定基准目录，不得隐式生成可点击文件引用',
);
assert.equal(
  fileReference.normalizeImplicitFileReferenceTarget('./SKILL.md'),
  './SKILL.md',
  '显式工作区相对路径应保持可点击',
);
assert.equal(
  fileReference.normalizeFileReferenceTarget('SKILL.md'),
  'SKILL.md',
  '显式 Markdown 链接仍允许使用工作区根文件名',
);

const ambiguous = fileReference.splitFileReferenceText('检查 SKILL.md 是否存在');
assert.deepEqual(
  ambiguous,
  [{ kind: 'text', text: '检查 SKILL.md 是否存在' }],
  '普通文本中的裸文件名不得伪装成工作区文件链接',
);

const qualified = fileReference.splitFileReferenceText('检查 ./SKILL.md 是否存在');
assert.deepEqual(
  qualified,
  [
    { kind: 'text', text: '检查 ' },
    { kind: 'file', text: './SKILL.md', target: './SKILL.md' },
    { kind: 'text', text: ' 是否存在' },
  ],
  '带明确相对基准的路径应继续生成文件链接',
);

console.log('file reference golden checks passed');
