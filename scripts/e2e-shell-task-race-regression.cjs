#!/usr/bin/env node
/**
 * Shell adapter 短命 task 收口竞态回归脚本
 *
 * 目标：
 * 1) 模拟 process_launch 返回后、adapter 挂监听前 task 已结束
 * 2) 验证 autoPreviewProcessOutput 仍能通过初始同步 readProcess 发出 frame/completed
 * 3) 验证 result.content 被 final-read 替换为终态 process_read 快照
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');
const MARKER = 'MAGI_ADAPTER_RACE_OK';

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function installVscodeStub() {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {},
        window: {},
        Uri: { file: (p) => ({ fsPath: p }) },
        Position: class Position { constructor(line, character) { this.line = line; this.character = character; } },
        Range: class Range { constructor(start, end) { this.start = start; this.end = end; } },
        Selection: class Selection { constructor(anchor, active) { this.anchor = anchor; this.active = active; } },
        RelativePattern: class RelativePattern { constructor(base, pattern) { this.baseUri = base; this.pattern = pattern; } },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

class FakeMessageHub {
  constructor() {
    this.traceId = 'session-shell-race';
    this.events = [];
  }
  getTraceId() { return this.traceId; }
  getRequestContext() { return undefined; }
  getRequestMessageId() { return undefined; }
  sendMessage() { return true; }
  sendUpdate() { return true; }
  data(type, payload) { this.events.push({ type, payload }); }
}

async function main() {
  const baseAdapterPath = path.join(OUT, 'llm', 'adapters', 'base-adapter.js');
  const normalizerPath = path.join(OUT, 'normalizer', 'base-normalizer.js');
  const toolManagerPath = path.join(OUT, 'tools', 'tool-manager.js');
  for (const file of [baseAdapterPath, normalizerPath, toolManagerPath]) {
    if (!fs.existsSync(file)) throw new Error(`缺少 out 编译产物: ${file}，请先执行 npm run compile`);
  }

  installVscodeStub();
  const { BaseLLMAdapter } = require(baseAdapterPath);
  const { BaseNormalizer } = require(normalizerPath);
  const { ToolManager } = require(toolManagerPath);

  class DummyNormalizer extends BaseNormalizer {
    constructor() {
      super({ agent: 'orchestrator', defaultSource: 'assistant' });
    }
    parseChunk() { return []; }
    finalizeContext() {}
    detectInteraction() { return null; }
  }

  class DummyAdapter extends BaseLLMAdapter {
    get agent() { return 'orchestrator'; }
    get role() { return 'orchestrator'; }
    async sendMessage() { throw new Error('not implemented'); }
    async interrupt() {}
    async preview(streamId, toolCall, result, executionContext) {
      return this.autoPreviewProcessOutput(streamId, toolCall, result, executionContext);
    }
  }

  const toolManager = new ToolManager({
    workspaceRoot: ROOT,
    permissions: { allowEdit: true, allowBash: true, allowWeb: true },
  });
  const messageHub = new FakeMessageHub();
  const adapter = new DummyAdapter(
    {},
    new DummyNormalizer(),
    toolManager,
    { baseUrl: '', apiKey: '', model: 'test-model', provider: 'openai', enabled: true },
    messageHub,
  );

  const executionContext = { workerId: 'orchestrator', role: 'orchestrator' };
  const toolCall = {
    id: 'shell-race-launch',
    name: 'process_launch',
    arguments: {
      command: `sleep 0.2; echo ${MARKER}`,
      run_mode: 'task',
      wait: false,
      cwd: ROOT,
    },
  };

  try {
    const launchResult = await toolManager.executeInternalTool(toolCall, undefined, executionContext);
    assert(launchResult.isError !== true, `process_launch 执行失败: ${launchResult.content}`);

    const launchJson = JSON.parse(launchResult.content);
    assert(Number.isInteger(launchJson.terminal_id), 'process_launch 未返回 terminal_id');
    assert(
      launchJson.status === 'running' || launchJson.status === 'starting' || launchJson.status === 'queued',
      `process_launch 未快返运行态，实际=${launchJson.status}`,
    );

    await sleep(600);

    const previewResult = { ...launchResult };
    await adapter.preview('stream-shell-race', toolCall, previewResult, executionContext);

    const started = messageHub.events.find((event) => event.type === 'terminalStreamStarted');
    const frames = messageHub.events.filter((event) => event.type === 'terminalStreamFrame');
    const completed = messageHub.events.find((event) => event.type === 'terminalStreamCompleted');
    assert(started, '未收到 terminalStreamStarted');
    assert(frames.length >= 1, '未收到 terminalStreamFrame');
    assert(completed, '未收到 terminalStreamCompleted，说明仍依赖瞬时 completed 事件');

    const frameWithMarker = frames.find((event) => String(event.payload?.output || '').includes(MARKER));
    assert(frameWithMarker, '初始同步 readProcess 未补回短命 task 输出');
    assert(frameWithMarker.payload?.delta === true, 'terminalStreamFrame 必须保持 delta=true');
    assert(completed.payload?.status === 'completed', `terminalStreamCompleted 状态异常: ${completed.payload?.status}`);
    assert(String(completed.payload?.output || '').includes(MARKER), 'terminalStreamCompleted 未携带终态输出');

    const finalRead = JSON.parse(previewResult.content);
    assert(finalRead.status === 'completed', `final-read 状态异常: ${finalRead.status}`);
    assert(String(finalRead.output || '').includes(MARKER), 'final-read 未覆盖为终态输出');

    console.log('\n=== shell task race regression ===');
    console.log(JSON.stringify({
      pass: true,
      terminalId: launchJson.terminal_id,
      frames: frames.length,
      completedStatus: completed.payload?.status,
      finalReadStatus: finalRead.status,
    }, null, 2));
    process.exit(0);
  } finally {
    try {
      toolManager.getShellExecutor()?.dispose?.();
    } catch {
      // ignore
    }
  }
}

main().catch((error) => {
  console.error('shell task race 回归失败:', error?.stack || error);
  process.exit(1);
});