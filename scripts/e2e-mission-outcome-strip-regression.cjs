#!/usr/bin/env node
/**
 * Mission Outcome 控制块剥离回归脚本
 *
 * 目标：
 * 1) 控制块不应渲染到 UI 输出
 * 2) 结构化状态应驱动完成判定
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

const originalModuleLoad = Module._load;
Module._load = function patchedModuleLoad(request, parent, isMain) {
  if (request === 'vscode') {
    return {
      workspace: {
        getConfiguration() {
          return {
            get(_key, fallback) { return fallback; },
            update() { return Promise.resolve(); },
          };
        },
      },
      ConfigurationTarget: { Global: 1 },
      Uri: {
        file(filePath) { return { fsPath: filePath, path: filePath, toString() { return filePath; } }; },
        joinPath(base, ...parts) {
          const basePath = base && typeof base.path === 'string' ? base.path : '';
          const resolved = path.join(basePath, ...parts);
          return { fsPath: resolved, path: resolved, toString() { return resolved; } };
        },
      },
      window: {},
      commands: { executeCommand() { return Promise.resolve(); } },
    };
  }
  return originalModuleLoad.call(this, request, parent, isMain);
};

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

class StubClient {
  constructor(response) {
    this.response = response;
    this.calls = 0;
    this.config = {
      baseUrl: 'http://localhost',
      apiKey: 'test',
      model: 'test',
      provider: 'openai',
      enabled: true,
    };
  }

  async streamMessage(params, onChunk) {
    this.calls += 1;
    onChunk({ type: 'content_delta', content: this.response });
    return {
      content: '',
      toolCalls: [],
      usage: { inputTokens: 0, outputTokens: 0 },
      stopReason: 'end_turn',
    };
  }

  async sendMessage() {
    return {
      content: '',
      toolCalls: [],
      usage: { inputTokens: 0, outputTokens: 0 },
      stopReason: 'end_turn',
    };
  }

  async testConnection() {
    return true;
  }

  async testConnectionFast() {
    return { success: true, modelExists: true };
  }
}

class StubToolManager {
  async getTools() { return []; }
  getSnapshotContext() { return { missionId: 'mission-test' }; }
  async execute(toolCall) {
    return { toolCallId: toolCall.id, content: '[]', isError: false };
  }
}

async function main() {
  const { OrchestratorLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-adapter.js'));
  const { CodexNormalizer } = loadCompiledModule(path.join('normalizer', 'codex-normalizer.js'));
  const { MessageHub } = loadCompiledModule(path.join('orchestrator', 'core', 'message-hub.js'));

  const response = [
    '已完成本轮执行。',
    '[[MISSION_OUTCOME]]',
    '{"status":"completed","next_steps":[]}',
    '[[/MISSION_OUTCOME]]',
  ].join('\n');

  const client = new StubClient(response);
  const normalizer = new CodexNormalizer({ agent: 'orchestrator', defaultSource: 'orchestrator' });
  const toolManager = new StubToolManager();
  const messageHub = new MessageHub('trace-test', { enabled: false });
  const adapter = new OrchestratorLLMAdapter({
    client,
    normalizer,
    toolManager,
    messageHub,
    config: client.config,
    systemPrompt: 'test',
    deepTask: true,
  });

  let renderedText = '';
  messageHub.on('unified:update', (update) => {
    if (update.appendText) {
      renderedText += update.appendText;
    }
  });

  await adapter.connect();
  adapter.setTempEnableToolCalls(true);
  await adapter.sendMessage('测试控制块剥离');

  const runtime = adapter.getLastRuntimeState();
  assert(client.calls === 1, `应仅触发 1 轮调用，实际: ${client.calls}`);
  assert(runtime.reason === 'completed', `应为 completed，实际: ${runtime.reason}`);
  assert(!renderedText.includes('[[MISSION_OUTCOME]]'), '控制块不应出现在 UI 输出');
  assert(!renderedText.includes('[[/MISSION_OUTCOME]]'), '控制块不应出现在 UI 输出');

  console.log('\n=== mission outcome strip regression ===');
  console.log(JSON.stringify({
    pass: true,
    calls: client.calls,
    reason: runtime.reason,
  }, null, 2));
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('mission outcome strip 回归失败:', error?.stack || error);
  process.exit(1);
});
