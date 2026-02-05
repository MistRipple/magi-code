/**
 * UI MessageClassifier 单元测试
 */

import { classifyMessage } from '../../ui/webview-svelte/src/lib/message-classifier';
import { MessageCategory as UIMessageCategory } from '../../ui/webview-svelte/src/types/message-routing';
import { MessageLifecycle, MessageType, MessageCategory, type StandardMessage, type MessageSource } from '../../protocol/message-protocol';
import { InteractionType } from '../../protocol/message-protocol';

declare const describe: (name: string, fn: () => void) => void;
declare const test: (name: string, fn: () => void | Promise<void>) => void;
declare const expect: any;

function createMessage(overrides: Partial<StandardMessage>): StandardMessage {
  return {
    id: overrides.id || 'msg-1',
    traceId: overrides.traceId || 'trace-1',
    category: overrides.category || MessageCategory.CONTENT,  // 🔧 统一消息通道：默认 CONTENT
    type: overrides.type || MessageType.TEXT,
    source: overrides.source || ('orchestrator' as MessageSource),
    agent: overrides.agent || 'claude',
    lifecycle: overrides.lifecycle || MessageLifecycle.STARTED,
    blocks: overrides.blocks || [],
    metadata: overrides.metadata || {},
    timestamp: overrides.timestamp || Date.now(),
    updatedAt: overrides.updatedAt || Date.now(),
    interaction: overrides.interaction,
  };
}

describe('MessageClassifier', () => {
  test('编排者计划消息应路由为 ORCHESTRATOR_PLAN', () => {
    const msg = createMessage({ type: MessageType.PLAN, source: 'orchestrator' });
    const result = classifyMessage(msg);
    expect(result.category).toBe(UIMessageCategory.ORCHESTRATOR_PLAN);
  });

  test('编排者派发指令应路由为 WORKER_INSTRUCTION', () => {
    const msg = createMessage({
      source: 'orchestrator',
      metadata: { dispatchToWorker: true, worker: 'codex' },
      agent: 'codex',
    });
    const result = classifyMessage(msg);
    expect(result.category).toBe(UIMessageCategory.WORKER_INSTRUCTION);
    expect(result.worker).toBe('codex');
  });

  test('Worker 思考应路由为 WORKER_THINKING', () => {
    const msg = createMessage({ source: 'worker', type: MessageType.THINKING, agent: 'claude' });
    const result = classifyMessage(msg);
    expect(result.category).toBe(UIMessageCategory.WORKER_THINKING);
    expect(result.worker).toBe('claude');
  });

  test('Worker 工具调用应路由为 WORKER_TOOL_USE', () => {
    const msg = createMessage({ source: 'worker', type: MessageType.TOOL_CALL, agent: 'gemini' });
    const result = classifyMessage(msg);
    expect(result.category).toBe(UIMessageCategory.WORKER_TOOL_USE);
    expect(result.worker).toBe('gemini');
  });

  test('Worker 子任务摘要卡片应路由为 TASK_SUMMARY_CARD', () => {
    // 方案 B：使用 MessageType.TASK_CARD 进行类型识别
    const msg = createMessage({
      source: 'orchestrator', // 🔧 修正：只有编排者可以发送状态卡片
      type: MessageType.TASK_CARD,
      agent: 'codex',
      metadata: { subTaskCard: { title: 'done' } },
    });
    const result = classifyMessage(msg);
    expect(result.category).toBe(UIMessageCategory.TASK_SUMMARY_CARD);
    expect(result.worker).toBe('codex');
  });

  test('普通文本消息应路由为 ORCHESTRATOR_RESPONSE', () => {
    const msg = createMessage({
      source: 'orchestrator',
      type: MessageType.TEXT,
    });
    const result = classifyMessage(msg);
    expect(result.category).toBe(UIMessageCategory.ORCHESTRATOR_RESPONSE);
  });

  test('交互请求应路由为对应的 INTERACTION 类型', () => {
    const msg = createMessage({
      source: 'orchestrator',
      type: MessageType.INTERACTION,
      interaction: {
        type: InteractionType.PLAN_CONFIRMATION,
        requestId: 'req-1',
        prompt: 'confirm plan',
        required: true,
      },
    });
    const result = classifyMessage(msg);
    expect(result.category).toBe(UIMessageCategory.INTERACTION_CONFIRMATION);
  });
});
