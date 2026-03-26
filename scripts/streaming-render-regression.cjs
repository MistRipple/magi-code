#!/usr/bin/env node
/* eslint-disable no-console */

const fs = require('fs');
const path = require('path');
const vm = require('vm');
const ts = require('typescript');

const ROOT = path.resolve(__dirname, '..');
const moduleCache = new Map();

function resolveModulePath(baseDir, specifier) {
  const normalized = path.resolve(baseDir, specifier);
  const candidates = [
    normalized,
    `${normalized}.ts`,
    `${normalized}.js`,
    `${normalized}.cjs`,
    `${normalized}.mjs`,
    path.join(normalized, 'index.ts'),
    path.join(normalized, 'index.js'),
    path.join(normalized, 'index.cjs'),
    path.join(normalized, 'index.mjs'),
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
      return candidate;
    }
  }
  throw new Error(`无法解析模块: ${specifier} (from ${baseDir})`);
}

function loadTsModule(filePath) {
  const absolutePath = path.resolve(filePath);
  if (moduleCache.has(absolutePath)) {
    return moduleCache.get(absolutePath);
  }

  if (!absolutePath.endsWith('.ts')) {
    const loaded = require(absolutePath);
    moduleCache.set(absolutePath, loaded);
    return loaded;
  }

  const source = fs.readFileSync(absolutePath, 'utf8');
  const transpiled = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      esModuleInterop: true,
      importsNotUsedAsValues: ts.ImportsNotUsedAsValues.Remove,
      resolveJsonModule: true,
    },
    fileName: absolutePath,
    reportDiagnostics: true,
  });

  if (transpiled.diagnostics && transpiled.diagnostics.length > 0) {
    const hasError = transpiled.diagnostics.some((diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error);
    if (hasError) {
      const formatted = transpiled.diagnostics
        .map((diagnostic) => {
          const message = ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n');
          if (!diagnostic.file || typeof diagnostic.start !== 'number') {
            return message;
          }
          const { line, character } = diagnostic.file.getLineAndCharacterOfPosition(diagnostic.start);
          return `${diagnostic.file.fileName}:${line + 1}:${character + 1} ${message}`;
        })
        .join('\n');
      throw new Error(`TS 转译失败:\n${formatted}`);
    }
  }

  const module = { exports: {} };
  const dirname = path.dirname(absolutePath);
  const localRequire = (specifier) => {
    if (specifier.startsWith('.')) {
      const resolved = resolveModulePath(dirname, specifier);
      return loadTsModule(resolved);
    }
    return require(specifier);
  };

  const wrapped = `(function (exports, require, module, __filename, __dirname) {\n${transpiled.outputText}\n})`;
  const script = new vm.Script(wrapped, { filename: absolutePath });
  const fn = script.runInThisContext();
  fn(module.exports, localRequire, module, absolutePath, dirname);
  moduleCache.set(absolutePath, module.exports);
  return module.exports;
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

const scenarios = [];

function scenario(name, fn) {
  scenarios.push({ name, fn });
}

const { preprocessMarkdown } = loadTsModule(
  path.join(ROOT, 'src/ui/webview-svelte/src/lib/markdown-utils.ts'),
);
const { mergeCompleteBlocksForFinalization } = loadTsModule(
  path.join(ROOT, 'src/ui/webview-svelte/src/lib/streaming-complete-merge.ts'),
);

scenario('转义换行仅在代码块外替换（支持 ``` 与 ~~~）', () => {
  const input = [
    '标题\\n换行',
    '```ts',
    'const a = \"X\\\\nY\";',
    '```',
    '~~~js',
    'const b = \"M\\\\nN\";',
    '~~~',
    '尾部\\n换行',
  ].join('\n');
  const output = preprocessMarkdown(input, false);
  assert(output.includes('标题\n换行'), '代码块外的 \\\\n 未被替换');
  assert(output.includes('尾部\n换行'), '尾部代码块外的 \\\\n 未被替换');
  assert(output.includes('const a = \"X\\\\nY\";'), '``` 代码块内的 \\\\n 被错误替换');
  assert(output.includes('const b = \"M\\\\nN\";'), '~~~ 代码块内的 \\\\n 被错误替换');
});

scenario('同行 fenced code 不应污染后续换行替换', () => {
  const input = [
    '```txt inline```',
    '后续\\n换行',
  ].join('\n');
  const output = preprocessMarkdown(input, false);
  assert(output.includes('后续\n换行'), '同行 fenced code 导致后续 \\\\n 替换失效');
});

scenario('流式未闭合 fence 自动补全闭合标记', () => {
  const tickFence = preprocessMarkdown('```ts\nconst value = 1', true);
  assert(tickFence.endsWith('\n```'), '``` 未闭合时未自动补全');
  const tildeFence = preprocessMarkdown('~~~bash\necho 1', true);
  assert(tildeFence.endsWith('\n~~~'), '~~~ 未闭合时未自动补全');
});

scenario('complete 收口保留新增结构化块且不重复 tool_call', () => {
  const existingBlocks = [
    {
      type: 'text',
      content: '已流式文本',
    },
    {
      type: 'tool_call',
      content: '',
      toolCall: {
        id: 'tool-1',
        name: 'worker_wait',
        status: 'running',
        arguments: '{}',
      },
    },
  ];

  const completeBlocks = [
    {
      type: 'text',
      content: '已流式文本',
    },
    {
      type: 'tool_call',
      content: '',
      toolCall: {
        id: 'tool-1',
        name: 'worker_wait',
        status: 'success',
        arguments: '{}',
        result: '{\"wait_status\":\"completed\"}',
      },
    },
    {
      type: 'plan',
      content: '计划',
      plan: {
        goal: '完成目标',
        analysis: '分析',
      },
    },
    {
      type: 'file_change',
      content: '',
      fileChange: {
        filePath: 'src/demo.ts',
        changeType: 'modify',
        diff: '@@ -1 +1 @@',
      },
    },
  ];

  const merged = mergeCompleteBlocksForFinalization(existingBlocks, completeBlocks, completeBlocks);
  assert(Array.isArray(merged), 'merge 结果必须是数组');
  assert(merged.some((block) => block.type === 'plan'), 'complete 新增 plan 块丢失');
  assert(merged.some((block) => block.type === 'file_change'), 'complete 新增 file_change 块丢失');
  const toolCallCount = merged.filter((block) => block.type === 'tool_call' && block.toolCall?.id === 'tool-1').length;
  assert(toolCallCount === 1, `tool_call 被重复追加，当前数量=${toolCallCount}`);
});

scenario('无流式已有块时回退到 complete/base 块', () => {
  const base = [{ type: 'text', content: 'base' }];
  const resolved = mergeCompleteBlocksForFinalization(undefined, undefined, base);
  assert(Array.isArray(resolved) && resolved.length === 1, 'base 回退失败');
  assert(resolved[0].type === 'text' && resolved[0].content === 'base', 'base 内容异常');
});

function run() {
  const failures = [];
  for (const item of scenarios) {
    try {
      item.fn();
      console.log(`✅ ${item.name}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      failures.push({ name: item.name, message });
      console.error(`❌ ${item.name}`);
      console.error(`   ${message}`);
    }
  }

  if (failures.length > 0) {
    console.error(`\n回归失败: ${failures.length}/${scenarios.length}`);
    process.exitCode = 1;
    return;
  }
  console.log(`\n回归通过: ${scenarios.length}/${scenarios.length}`);
}

run();
