import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const { MessageScrollCoordinator, MessageLayoutStabilizer } = await server.ssrLoadModule(
    '/src/lib/message-scroll-coordinator.ts',
  );
  const coordinator = new MessageScrollCoordinator();
  const first = coordinator.begin(200, 1, 'thread', 'restore');
  assert.equal(coordinator.pending, true, '应用写入后必须存在待确认事务');
  assert.equal(coordinator.consumeIntentIfMatches(200, 1, 'thread'), 'restore', '目标位置事件必须确认当前事务');
  assert.equal(coordinator.pending, false, '确认后事务必须立即结束');
  assert.equal(coordinator.consumeIntentIfMatches(201, 1, 'thread'), null, '确认后的后续事件必须按用户事件处理');

  const second = coordinator.begin(300, 2, 'thread', 'navigation');
  assert.notEqual(second.id, first.id, '每次应用滚动必须拥有新的事务代际');
  assert.equal(coordinator.consumeIntentIfMatches(292, 2, 'thread'), null, '偏离目标的事件不能确认应用滚动');
  assert.equal(coordinator.pending, true, '无法判定来源的延迟事件不得取消最新事务');
  assert.equal(coordinator.consumeIntentIfMatches(300, 2, 'thread'), 'navigation', '最新目标事件仍必须确认应用滚动');
  coordinator.cancel();

  coordinator.begin(400, 3, 'thread', 'follow-bottom');
  assert.equal(coordinator.consumeIntentIfMatches(400, 4, 'thread'), null, '交互代际变化必须使旧事务失效');
  assert.equal(coordinator.pending, false, '交互代际变化不得保留旧事务');

  const unchanged = coordinator.begin(400, 4, 'thread', 'restore');
  assert.equal(coordinator.completeIfUnchanged(unchanged.id, 400, 400), 'restore', '没有位置变化时必须同步完成事务');
  assert.equal(coordinator.pending, false, '没有 scroll 事件时事务也不得残留');

  const repeated = coordinator.begin(450, 4, 'thread', 'follow-bottom');
  assert.equal(coordinator.completeIfUnchanged(repeated.id, 400, 450), null, '发生位置变化时必须等待目标事件');
  assert.equal(coordinator.completeIfUnchanged(repeated.id, 450, 450), null, '重复写入不能提前消费尚未到达的旧目标事件');
  assert.equal(coordinator.pending, true, '重复写入后事务必须继续等待目标事件');
  assert.equal(coordinator.consumeIntentIfMatches(450, 4, 'thread'), 'follow-bottom', '重复写入后的目标事件仍必须确认事务');

  const changed = coordinator.begin(500, 5, 'thread', 'navigation');
  assert.equal(coordinator.completeIfUnchanged(changed.id, 450, 500), null, '发生位置变化时必须等待浏览器 scroll 事件');
  assert.equal(coordinator.pending, true, '发生位置变化时事务必须等待目标事件');
  assert.equal(coordinator.consumeIntentIfMatches(500, 5, 'thread'), 'navigation', '变化后的目标事件仍必须确认应用滚动');

  coordinator.begin(500, 5, 'thread', 'restore');
  assert.equal(coordinator.pending, true, '延迟 scroll 事件到达前事务不得按时间过期');
  assert.equal(coordinator.consumeIntentIfMatches(500, 5, 'thread'), 'restore', '延迟目标事件仍必须被识别为应用滚动');

  const latest = coordinator.begin(600, 6, 'thread', 'restore');
  const retargeted = coordinator.begin(700, 6, 'thread', 'restore');
  assert.equal(coordinator.consumeIntentIfMatches(600, 6, 'thread'), null, '旧事务不能确认最新事务');
  assert.equal(coordinator.isPendingFor(6, 'thread'), true, '当前作用域必须能识别待确认事务');
  const newest = coordinator.begin(700, 6, 'thread', 'navigation');
  assert.equal(coordinator.consumeIntentIfMatches(700.9, 6, 'thread'), 'navigation', '最新事务允许小范围浮点误差');
  assert.equal(latest.id, retargeted.id, '同一滚动上下文必须复用事务并更新目标');
  assert.equal(latest.id < newest.id, true, '不同意图必须创建新的事务代际');

  coordinator.begin(800, 7, 'thread', 'restore');
  assert.equal(coordinator.consumeIntentIfMatches(800, 7, 'task:other'), null, '跨面板事件不能确认当前事务');
  assert.equal(coordinator.pending, false, '跨面板事件必须使旧事务失效');

  const stabilizer = new MessageLayoutStabilizer();
  const context = { scopeKey: 'thread', interactionEpoch: 1, recoveryEpoch: 1 };
  stabilizer.remember({ messageId: 'visible', offsetTop: 80 }, context);
  assert.equal(
    stabilizer.compensate({ messageId: 'visible', offsetTop: 116 }, 240, context),
    276,
    '锚点上方内容增高时必须补偿相同的滚动位移',
  );
  assert.equal(
    stabilizer.compensate({ messageId: 'other', offsetTop: 116 }, 240, context),
    null,
    '可见锚点更换时不能猜测滚动位移',
  );
  assert.equal(
    stabilizer.compensate(
      { messageId: 'visible', offsetTop: 116 },
      240,
      { ...context, recoveryEpoch: 2 },
    ),
    null,
    '用户新交互后必须使旧布局恢复事务失效',
  );
  stabilizer.remember(null, context);
  assert.equal(
    stabilizer.compensate({ messageId: 'visible', offsetTop: 116 }, 240, context),
    null,
    '没有可见锚点时必须清除旧锚点，不能复用上一次布局事实',
  );
});

console.log('message-scroll-coordinator golden checks passed');
