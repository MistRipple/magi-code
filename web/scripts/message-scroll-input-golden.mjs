import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const {
    canScrollableElementConsumeWheel,
    deriveMessageScrollDirection,
    isMainMessageWheelInput,
    shouldFollowMessageScrollInput,
  } = await server.ssrLoadModule('/src/lib/message-scroll-input.ts');

  assert.equal(deriveMessageScrollDirection(100, 120), 'up');
  assert.equal(deriveMessageScrollDirection(120, 100), 'down');
  assert.equal(deriveMessageScrollDirection(100.5, 100), 'none');

  assert.equal(shouldFollowMessageScrollInput('down', { scrollTop: 0, scrollHeight: 500, clientHeight: 500 }), true,
    '无溢出时向下输入没有 scroll 事件，仍须保持自动跟随');
  assert.equal(shouldFollowMessageScrollInput('down', { scrollTop: 500, scrollHeight: 1000, clientHeight: 500 }), true,
    '已到底部时向下输入必须恢复跟随');
  assert.equal(shouldFollowMessageScrollInput('up', { scrollTop: 500, scrollHeight: 1000, clientHeight: 500 }), false,
    '主动向上阅读仍暂停跟随');
  assert.equal(shouldFollowMessageScrollInput('down', { scrollTop: 459, scrollHeight: 1000, clientHeight: 500 }), false);
  assert.equal(shouldFollowMessageScrollInput('down', { scrollTop: 460, scrollHeight: 1000, clientHeight: 500 }), true);
  assert.equal(shouldFollowMessageScrollInput('down', { scrollTop: NaN, scrollHeight: 1000, clientHeight: 500 }), false);

  const scrollable = { scrollTop: 40, scrollHeight: 320, clientHeight: 100 };
  assert.equal(canScrollableElementConsumeWheel(-20, scrollable, 'auto'), true);
  assert.equal(canScrollableElementConsumeWheel(20, scrollable, 'auto'), true);
  assert.equal(canScrollableElementConsumeWheel(-20, { ...scrollable, scrollTop: 0 }, 'auto'), false);
  assert.equal(canScrollableElementConsumeWheel(20, { ...scrollable, scrollTop: 220 }, 'auto'), false);
  assert.equal(canScrollableElementConsumeWheel(-20, scrollable, 'hidden'), false);

  assert.equal(isMainMessageWheelInput({
    deltaY: -30,
    ctrlKey: false,
    metaKey: false,
    nestedScrollerCanConsume: false,
  }), true);
  assert.equal(isMainMessageWheelInput({
    deltaY: -30,
    ctrlKey: true,
    metaKey: false,
    nestedScrollerCanConsume: false,
  }), false);
  assert.equal(isMainMessageWheelInput({
    deltaY: -30,
    ctrlKey: false,
    metaKey: false,
    nestedScrollerCanConsume: true,
  }), false);
});

console.log('message-scroll-input golden checks passed');
