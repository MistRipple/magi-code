#!/usr/bin/env node
/**
 * code_search_semantic 本地检索链路回归脚本
 *
 * 目标：
 * 1. 验证 code_search_semantic 在无外部检索配置情况下可直接执行
 * 2. 验证 scope_paths 参数可被正确接收
 * 3. 验证输出为本地检索语义（PKB + Grep + LSP）
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function installVscodeStub() {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {
          workspaceFolders: [],
          getConfiguration: () => ({ get: () => undefined }),
          fs: {
            stat: async () => ({}),
            readDirectory: async () => [],
            readFile: async () => Buffer.from(''),
          },
          findFiles: async () => [],
          openTextDocument: async () => ({
            uri: { fsPath: '', toString: () => '' },
            getText: () => '',
            positionAt: () => ({ line: 0, character: 0 }),
            lineAt: () => ({ text: '' }),
            languageId: 'typescript',
          }),
        },
        window: {
          createOutputChannel: () => ({ appendLine() {}, append() {}, clear() {}, show() {}, dispose() {} }),
          showErrorMessage: async () => undefined,
          showWarningMessage: async () => undefined,
          showInformationMessage: async () => undefined,
          onDidCloseTerminal: () => ({ dispose() {} }),
          onDidOpenTerminal: () => ({ dispose() {} }),
          createTerminal: () => ({ sendText() {}, show() {}, dispose() {} }),
          terminals: [],
          activeTextEditor: undefined,
          visibleTextEditors: [],
        },
        commands: {
          executeCommand: async () => undefined,
          registerCommand: () => ({ dispose() {} }),
        },
        languages: {
          getDiagnostics: () => [],
        },
        env: {
          shell: process.env.SHELL || '/bin/zsh',
          clipboard: {
            readText: async () => '',
            writeText: async () => {},
          },
        },
        Uri: {
          file: (p) => ({ fsPath: p, path: p, toString: () => p }),
          parse: (p) => ({ fsPath: p, path: p, toString: () => p }),
          joinPath: (...parts) => ({
            fsPath: parts.map(p => (typeof p === 'string' ? p : p.path || '')).join('/'),
            toString() { return this.fsPath; },
          }),
        },
        EventEmitter: class {
          constructor() {
            this.listeners = new Set();
            this.event = (listener) => {
              this.listeners.add(listener);
              return { dispose: () => this.listeners.delete(listener) };
            };
          }
          fire(data) {
            for (const listener of this.listeners) {
              try { listener(data); } catch {}
            }
          }
          dispose() { this.listeners.clear(); }
        },
        Disposable: class { dispose() {} },
        Position: class { constructor(line, character) { this.line = line; this.character = character; } },
        Range: class { constructor(start, end) { this.start = start; this.end = end; } },
        Selection: class { constructor(anchor, active) { this.anchor = anchor; this.active = active; } },
        RelativePattern: class { constructor(base, pattern) { this.baseUri = base; this.pattern = pattern; } },
        ViewColumn: { One: 1, Two: 2, Three: 3 },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

function extractKeywords(query) {
  return String(query)
    .split(/[\s,，。.!！?？;；:：()（）[\]【】{}]+/)
    .map(w => w.trim())
    .filter(Boolean)
    .filter(w => w.length >= 2)
    .slice(0, 10);
}

async function main() {
  if (!fs.existsSync(path.join(OUT, 'tools', 'tool-manager.js'))) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  installVscodeStub();

  const { ToolManager } = require(path.join(OUT, 'tools', 'tool-manager.js'));
  const { CodebaseRetrievalService } = require(path.join(OUT, 'services', 'codebase-retrieval-service.js'));

  const workspaceFolders = [{ name: path.basename(ROOT), path: ROOT }];
  const toolManager = new ToolManager({
    workspaceRoot: ROOT,
    workspaceFolders,
    permissions: { allowEdit: false, allowBash: false, allowWeb: false },
  });

  const retrievalService = new CodebaseRetrievalService({
    getKnowledgeBase: () => undefined,
    executeTool: async (toolCall) => {
      if (toolCall.name === 'code_intel_query') {
        return toolManager.getLspExecutor().execute(toolCall);
      }
      return toolManager.execute(toolCall);
    },
    extractKeywords,
    workspaceFolders,
  });

  toolManager.getCodebaseRetrievalExecutor().setCodebaseRetrievalService(retrievalService);

  const fullSearchResult = await toolManager.execute({
    id: 'retrieval-full',
    name: 'code_search_semantic',
    arguments: {
      query: 'ToolManager executeBuiltinTool code_search_semantic',
      max_results: 5,
    },
  });

  const scopedSearchResult = await toolManager.execute({
    id: 'retrieval-scoped',
    name: 'code_search_semantic',
    arguments: {
      query: 'WebviewProvider injectCodebaseRetrievalService',
      scope_paths: ['src/ui', 'src/tools'],
      max_results: 5,
    },
  });

  const fullContent = String(fullSearchResult.content || '');
  const scopedContent = String(scopedSearchResult.content || '');
  const fullContentValid = fullContent.includes('Searched via local codebase retrieval')
    || fullContent.includes('未找到相关代码（本地检索无结果）');
  const scopedContentValid = scopedContent.includes('Searched via local codebase retrieval')
    || scopedContent.includes('未找到相关代码（本地检索无结果）');

  const pass = !fullSearchResult.isError
    && !scopedSearchResult.isError
    && fullContentValid
    && scopedContentValid;

  console.log('\n=== code_search_semantic 回归结果 ===');
  console.log(JSON.stringify({
      fullSearchError: !!fullSearchResult.isError,
      fullSearchPreview: fullContent.slice(0, 220),
      scopedSearchError: !!scopedSearchResult.isError,
      scopedSearchPreview: scopedContent.slice(0, 220),
      pass,
    }, null, 2));

  process.exit(pass ? 0 : 2);
}

main().catch(error => {
  console.error('code_search_semantic 回归失败:', error?.stack || error);
  process.exitCode = 1;
});
