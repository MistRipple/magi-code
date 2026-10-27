import assert from 'node:assert/strict';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import { INSTALL_PAGE_RUNTIME } from './page-script.js';

function pointerFixture() {
  const listeners = new Map<string, Set<(event: unknown) => void>>();
  let scrolls = 0;
  let covered = false;
  const rect = { x: 10, y: 20, left: 10, top: 20, right: 110, bottom: 60, width: 100, height: 40 };
  const view = { innerWidth: 800, innerHeight: 600, frameElement: null, getComputedStyle: () => ({ display: 'block', visibility: 'visible', opacity: '1' }) };
  const doc = {
    defaultView: view,
    elementFromPoint: () => covered ? null : element,
    addEventListener(type: string, listener: (event: unknown) => void) {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type)!.add(listener);
    },
    removeEventListener(type: string, listener: (event: unknown) => void) { listeners.get(type)?.delete(listener); },
  };
  const element = {
    nodeType: 1, tagName: 'BUTTON', isConnected: true, ownerDocument: doc,
    getAttribute: (name: string) => name === 'aria-label' ? 'Target' : null,
    matches: () => false, closest: () => null, getRootNode: () => doc,
    getBoundingClientRect: () => rect, scrollIntoView: () => { scrolls++; },
  };
  const timers = new Map<number, () => void>();
  let nextTimer = 0;
  const runtime = runInNewContext(`${INSTALL_PAGE_RUNTIME}('test'); globalThis.__magiBrowserAutomation`, {
    innerWidth: 800, innerHeight: 600,
    setTimeout: (callback: () => void) => { timers.set(++nextTimer, callback); return nextTimer; },
    clearTimeout: (id: number) => timers.delete(id),
  });
  const prepare = (event = 'click') => runtime.preparePointer(runtime.reference(element), 'pointer', event, null);
  const fire = (trusted: boolean, target = element, event = 'click') => {
    for (const listener of listeners.get(event) ?? []) listener({ isTrusted: trusted, composedPath: () => [target, doc] });
  };
  return { runtime, prepare, fire, timers, listeners, element, rect, scrolls: () => scrolls, cover: () => { covered = true; } };
}

test('迟到的可信 click 在观察窗口内确认，错误目标和合成事件不会放行', async () => {
  const f = pointerFixture();
  f.prepare();
  let completed = false;
  const result = f.runtime.finishPointer('pointer', true).then((value: unknown) => { completed = true; return value; });
  f.fire(false);
  f.fire(true, {} as typeof f.element);
  await Promise.resolve();
  assert.equal(completed, false);
  f.fire(true);
  assert.equal((await result).observed, true);
  assert.equal(f.timers.size, 0);
  assert.equal(f.listeners.get('click')?.size, 0);
});

test('已观察到的 click 无等待；未到达事件超时仍返回未确认并清理监听', async () => {
  const f = pointerFixture();
  f.prepare();
  f.fire(true);
  assert.equal((await f.runtime.finishPointer('pointer', true)).observed, true);
  assert.equal(f.timers.size, 0);
  f.prepare();
  const result = f.runtime.finishPointer('pointer', true);
  for (const timeout of f.timers.values()) timeout();
  assert.equal((await result).observed, false);
  assert.equal(f.listeners.get('click')?.size, 0);
});

test('已经可命中的点击/输入目标保持页面滚动位置，离屏才滚入视口', () => {
  const f = pointerFixture();
  f.prepare();
  assert.equal(f.scrolls(), 0);
  f.rect.top = f.rect.y = -100;
  f.rect.bottom = -60;
  assert.throws(() => f.prepare(), /browser_target_outside_viewport/);
  assert.equal(f.scrolls(), 1);
});

test('悬停在可信移动后重新确认命中，目标移走或被遮挡不能报告成功', async () => {
  const f = pointerFixture();
  f.prepare('mousemove');
  f.fire(true, f.element, 'mousemove');
  assert.equal((await f.runtime.finishPointer('pointer', true)).observed, true);
  f.prepare('mousemove');
  f.fire(true, f.element, 'mousemove');
  f.cover();
  await assert.rejects(f.runtime.finishPointer('pointer', true), /browser_target_obscured/);
  assert.equal(f.listeners.get('mousemove')?.size, 0);
  assert.equal((await f.runtime.finishPointer('pointer')).observed, false);
});
