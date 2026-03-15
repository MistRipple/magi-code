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

  console.log(JSON.stringify({
    pass: true,
    checks: [
      'followup-request-context-bridge',
      'followup-execution-round-guardrails',
    ],
  }, null, 2));
}

main();
