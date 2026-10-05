import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const overlaySource = await readFile(
  new URL('../src/components/tabs/AgentCursorOverlay.svelte', import.meta.url),
  'utf8',
);

// 覆盖层必须位于 webview guest 之上且不拦截用户输入，否则用户无法接管页面。
assert.match(overlaySource, /popover="manual"/, '光标覆盖层必须使用 Top Layer 才能盖住 webview guest');
assert.match(overlaySource, /\.agent-cursor-layer \{[\s\S]*?pointer-events: none;/, '光标覆盖层不得拦截用户对网页的操作');
assert.match(overlaySource, /\.agent-cursor-layer \{[\s\S]*?width: anchor-size\(width\);[\s\S]*?height: anchor-size\(height\);/, '覆盖层尺寸必须贴合内容槽');
assert.match(overlaySource, /aria-hidden="true"/, '光标是装饰性反馈，状态由工具栏徽标播报');
assert.match(overlaySource, /@media \(prefers-reduced-motion: reduce\)/, '必须尊重减少动态效果设置');

await withGoldenViteServer(async (server) => {
  const cursor = await server.ssrLoadModule('/src/lib/agent-cursor.ts');

  assert.equal(cursor.normalizeAgentCursorAction('click'), 'click');
  assert.equal(cursor.normalizeAgentCursorAction('hover'), null);
  assert.equal(cursor.agentCursorPoint(10, 20)?.x, 10);
  assert.equal(cursor.agentCursorPoint(null, 20), null);
  assert.equal(cursor.agentCursorPoint(Number.NaN, 20), null);

  // 首次出现与原地更新不滑行；距离越远越久，但有上下限。
  assert.equal(cursor.agentCursorGlideMillis(null, { x: 100, y: 100 }), 0);
  assert.equal(cursor.agentCursorGlideMillis({ x: 100, y: 100 }, { x: 101, y: 100 }), 0);
  const near = cursor.agentCursorGlideMillis({ x: 0, y: 0 }, { x: 30, y: 40 });
  const far = cursor.agentCursorGlideMillis({ x: 0, y: 0 }, { x: 600, y: 800 });
  const veryFar = cursor.agentCursorGlideMillis({ x: 0, y: 0 }, { x: 6000, y: 8000 });
  assert.ok(near >= 160 && near < far, `近距离滑行应短于远距离：${near} / ${far}`);
  assert.equal(far, 560);
  assert.equal(veryFar, 560);

  // 名牌靠近右侧或底部时翻到另一侧，保持在内容槽内。
  const box = { width: 800, height: 600 };
  assert.deepEqual(cursor.agentCursorTagPlacement({ x: 100, y: 100 }, box), { flipX: false, flipY: false });
  assert.deepEqual(cursor.agentCursorTagPlacement({ x: 760, y: 590 }, box), { flipX: true, flipY: true });

  assert.equal(cursor.agentCursorActionLabelKey('move'), null);
  assert.equal(cursor.agentCursorActionLabelKey('type'), 'browser.agent.cursor.type');
  assert.equal(cursor.isStationaryMove(null, { x: 1, y: 1 }), false);
});

console.log('agent cursor golden tests passed');
