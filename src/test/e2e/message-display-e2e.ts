/**
 * Message Display E2E (逻辑层验证)
 *
 * 目标：验证消息分类 + 路由 + 展示位置的核心约束
 */

import { routeStandardMessage } from '../../ui/webview-svelte/src/lib/message-router';
import { MessageLifecycle, MessageType, type StandardMessage } from '../../protocol/message-protocol';

declare const describe: (name: string, fn: () => void) => void;
declare const test: (name: string, fn: () => void | Promise<void>) => void;
declare const expect: any;

function createMessage(overrides: Partial<StandardMessage>): StandardMessage {
  return {
    id: overrides.id || `msg-${Date.now()}`,
    traceId: overrides.traceId || 'trace',
    type: overrides.type || MessageType.TEXT,
    source: overrides.source || 'orchestrator',
    agent: overrides.agent || 'claude',
    lifecycle: overrides.lifecycle || MessageLifecycle.STARTED,
    blocks: overrides.blocks || [],
    metadata: overrides.metadata || {},
    timestamp: overrides.timestamp || Date.now(),
    updatedAt: overrides.updatedAt || Date.now(),
    interaction: overrides.interaction,
  };
}

describe('Message Display Flow', () => {
  test('编排者派发指令应同时显示在主对话与 Worker', () => {
    const msg = createMessage({
      source: 'orchestrator',
      agent: 'codex',
      metadata: { dispatchToWorker: true, worker: 'codex' },
    });
    const target = routeStandardMessage(msg);
    expect(target.location).toBe('both');
    if (target.location === 'both') {
      expect(target.worker).toBe('codex');
    }
  });

  test('Worker 输出应仅显示在 Worker 面板', () => {
    const msg = createMessage({
      source: 'worker',
      agent: 'gemini',
      type: MessageType.TEXT,
    });
    const target = routeStandardMessage(msg);
    expect(target.location).toBe('worker');
    if (target.location === 'worker') {
      expect(target.worker).toBe('gemini');
    }
  });

  test('编排者计划应仅显示在主对话区', () => {
    const msg = createMessage({
      source: 'orchestrator',
      type: MessageType.PLAN,
    });
    const target = routeStandardMessage(msg);
    expect(target.location).toBe('thread');
  });
});
