#!/usr/bin/env node
/**
 * 续跑过程消息可见性回归
 *
 * 目标：
 * 1) normalizer -> MessageHub 链路必须为续跑轮显式透传当前 requestId
 * 2) 自动续跑执行轮约束仍然存在，避免只说“准备派发”但不落工具
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function main() {
  const baseAdapterSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapters', 'base-adapter.ts'),
    'utf8',
  );
  const missionEngineSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts'),
    'utf8',
  );
  const messageHandlerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.ts'),
    'utf8',
  );

  assert(
    baseAdapterSource.includes('sendWithCurrentRequestContext'),
    'BaseAdapter 缺少续跑过程消息 requestId 透传桥接',
  );
  assert(
    baseAdapterSource.includes('explicitRequestId: this.currentRequestId'),
    'BaseAdapter 未向 MessageHub 显式透传当前 requestId',
  );
  assert(
    missionEngineSource.includes('这是执行轮，不是规划轮。'),
    '自动续跑执行轮约束缺失',
  );
  assert(
    missionEngineSource.includes('必要时调用工具或 worker_dispatch / worker_wait 继续推进'),
    '自动续跑未强制要求通过工具继续推进',
  );
  assert(
    missionEngineSource.includes('beginSyntheticExecutionRound'),
    'MissionDrivenEngine 缺少插件发起轮次的独立 requestId 入口',
  );
  assert(
    missionEngineSource.includes('currentRoundRequestId = this.beginSyntheticExecutionRound'),
    '自动续跑未切换到独立的插件轮次 requestId',
  );
  const messageFactorySource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'core', 'message-factory.ts'),
    'utf8',
  );
  assert(
    messageFactorySource.includes('beginSyntheticRound(requestId'),
    'MessageFactory 缺少插件发起轮次的起始消息/placeholder 创建逻辑',
  );
  assert(
    messageFactorySource.includes('placeholderState: \'pending\''),
    '插件发起轮次未创建独立 placeholder',
  );
  const orchestratorAdapterSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapters', 'orchestrator-adapter.ts'),
    'utf8',
  );
  assert(
    orchestratorAdapterSource.includes("chunk.type === 'tool_call_start' && chunk.toolCall"),
    'Orchestrator 缺少 tool_call_start 即时可见性入口',
  );
  assert(
    orchestratorAdapterSource.includes('this.normalizer.addToolCall(streamId'),
    'Orchestrator 未在 tool_call_start 时立即投影工具卡片',
  );
  assert(
    orchestratorAdapterSource.includes('const usesPersistentVisibleStream = visibility !== \'system\';'),
    'Orchestrator 缺少同轮可见消息聚合开关',
  );
  assert(
    orchestratorAdapterSource.includes(': persistentVisibleStreamId!;'),
    'Orchestrator 续跑轮未复用同一可见 stream',
  );
  const workerAdapterSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapters', 'worker-adapter.ts'),
    'utf8',
  );
  assert(
    workerAdapterSource.includes('const persistentVisibleStreamId = this.startStreamWithContext();'),
    'Worker 缺少同轮可见消息聚合入口',
  );
  assert(
    workerAdapterSource.includes('const streamId = persistentVisibleStreamId;'),
    'Worker 续跑/工具循环未复用同一可见 stream',
  );
  assert(
    messageHandlerSource.includes('const lastBlock = nextBlocks[nextBlocks.length - 1];'),
    '前端 appendText 未按尾部 block 顺序追加',
  );
  assert(
    messageHandlerSource.includes("if (prev?.type === 'text')"),
    '前端 block 合并未按尾部 text block 保序',
  );

  console.log(JSON.stringify({
    pass: true,
    checks: [
      'followup-request-context-bridge',
      'followup-execution-round-guardrails',
      'followup-synthetic-round-request',
      'followup-tool-call-start-visibility',
      'followup-single-stream-aggregation',
      'followup-block-order-preservation',
    ],
  }, null, 2));
}

main();
