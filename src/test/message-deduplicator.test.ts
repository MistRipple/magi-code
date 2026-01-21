/**
 * MessageDeduplicator 单元测试
 */

import { MessageDeduplicator } from '../normalizer/message-deduplicator';
import { StandardMessage, MessageLifecycle, MessageType, MessageSource } from '../protocol';

declare const describe: (name: string, fn: () => void) => void;
declare const test: (name: string, fn: () => void | Promise<void>) => void;
declare const beforeEach: (fn: () => void) => void;
declare const afterEach: (fn: () => void) => void;
declare const expect: any;

function createTestMessage(
  id: string,
  lifecycle: MessageLifecycle,
  source: MessageSource = 'worker'
): StandardMessage {
  return {
    id,
    traceId: 'test-trace',
    type: 'text' as MessageType,
    source,
    agent: 'claude',  // ✅ 使用 agent
    lifecycle,
    blocks: [],
    metadata: { extra: { priority: 0, tags: [] } },
    timestamp: Date.now(),
    updatedAt: Date.now(),
  };
}

describe('MessageDeduplicator', () => {
  let deduplicator: MessageDeduplicator;

  beforeEach(() => {
    deduplicator = new MessageDeduplicator({
      enabled: true,
      minStreamInterval: 100,
      retentionTime: 5000,
      maxHistorySize: 100,
    });
  });

  afterEach(() => {
    deduplicator.reset();
  });

  test('应该发送 STARTED 消息', () => {
    const message = createTestMessage('msg-1', MessageLifecycle.STARTED);
    expect(deduplicator.shouldSend(message)).toBe(true);
  });

  test('应该发送新消息', () => {
    const message = createTestMessage('msg-2', MessageLifecycle.STREAMING);
    expect(deduplicator.shouldSend(message)).toBe(true);
  });

  test('应该跳过已完成的消息', () => {
    const message1 = createTestMessage('msg-3', MessageLifecycle.STARTED);
    deduplicator.shouldSend(message1); // 发送 STARTED

    const message2 = createTestMessage('msg-3', MessageLifecycle.COMPLETED);
    deduplicator.shouldSend(message2); // 发送 COMPLETED

    const message3 = createTestMessage('msg-3', MessageLifecycle.STREAMING);
    expect(deduplicator.shouldSend(message3)).toBe(false); // 应该跳过
  });

  test('应该限制 STREAMING 消息的发送间隔', async () => {
    const message1 = createTestMessage('msg-4', MessageLifecycle.STARTED);
    deduplicator.shouldSend(message1);

    const message2 = createTestMessage('msg-4', MessageLifecycle.STREAMING);
    expect(deduplicator.shouldSend(message2)).toBe(true); // 第一次流式更新

    // 立即发送第二次更新，应该被跳过（间隔不足 100ms）
    const message3 = createTestMessage('msg-4', MessageLifecycle.STREAMING);
    expect(deduplicator.shouldSend(message3)).toBe(false);

    // 等待 100ms 后再发送，应该通过
    await new Promise(resolve => setTimeout(resolve, 110));
    const message4 = createTestMessage('msg-4', MessageLifecycle.STREAMING);
    expect(deduplicator.shouldSend(message4)).toBe(true);
  });

  test('应该隔离不同 source 的消息', () => {
    const message1 = createTestMessage('msg-5', MessageLifecycle.STARTED, 'orchestrator');
    const message2 = createTestMessage('msg-5', MessageLifecycle.STARTED, 'worker');

    // 不同 source 的相同 ID 消息应该都能发送
    expect(deduplicator.shouldSend(message1)).toBe(true);
    expect(deduplicator.shouldSend(message2)).toBe(true);
  });

  test('getMessagesBySource 应该返回正确的消息', () => {
    const msg1 = createTestMessage('msg-6', MessageLifecycle.STARTED, 'orchestrator');
    const msg2 = createTestMessage('msg-7', MessageLifecycle.STARTED, 'worker');
    const msg3 = createTestMessage('msg-8', MessageLifecycle.STARTED, 'orchestrator');

    deduplicator.shouldSend(msg1);
    deduplicator.shouldSend(msg2);
    deduplicator.shouldSend(msg3);

    const orchestratorMessages = deduplicator.getMessagesBySource('orchestrator');
    expect(orchestratorMessages.length).toBe(2);
    expect(orchestratorMessages.map(m => m.id)).toEqual(['msg-6', 'msg-8']);

    const workerMessages = deduplicator.getMessagesBySource('worker');
    expect(workerMessages.length).toBe(1);
    expect(workerMessages[0].id).toBe('msg-7');
  });

  test('getStats 应该返回统计信息', () => {
    const msg1 = createTestMessage('msg-9', MessageLifecycle.STARTED, 'orchestrator');
    const msg2 = createTestMessage('msg-10', MessageLifecycle.COMPLETED, 'worker');

    deduplicator.shouldSend(msg1);
    deduplicator.shouldSend(msg2);

    const stats = deduplicator.getStats();
    expect(stats.totalMessages).toBe(2);
    expect(stats.completedMessages).toBe(1);
    expect(stats.sourceBreakdown.orchestrator).toBe(1);
    expect(stats.sourceBreakdown.worker).toBe(1);
  });

  test('禁用去重时应该总是发送', () => {
    const disabledDeduplicator = new MessageDeduplicator({ enabled: false });

    const message1 = createTestMessage('msg-11', MessageLifecycle.STARTED);
    const message2 = createTestMessage('msg-11', MessageLifecycle.COMPLETED);
    const message3 = createTestMessage('msg-11', MessageLifecycle.STREAMING);

    expect(disabledDeduplicator.shouldSend(message1)).toBe(true);
    expect(disabledDeduplicator.shouldSend(message2)).toBe(true);
    expect(disabledDeduplicator.shouldSend(message3)).toBe(true);
  });
});
