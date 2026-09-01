import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const {
    canScrollableElementConsumeWheel,
    deriveMessageScrollDirection,
    isMainMessageWheelInput,
  } = await server.ssrLoadModule('/src/lib/message-scroll-input.ts');

  assert.equal(deriveMessageScrollDirection(100, 120), 'up');
  assert.equal(deriveMessageScrollDirection(120, 100), 'down');
  assert.equal(deriveMessageScrollDirection(100.5, 100), 'none');

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
