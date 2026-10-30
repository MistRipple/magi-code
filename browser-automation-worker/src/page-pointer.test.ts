import assert from 'node:assert/strict';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import { INSTALL_PAGE_RUNTIME } from './page-script.js';

function pointerFixture(options: { observe?: boolean } = {}) {
  const observers: Array<{ callback: (records: unknown[]) => void; disconnected: boolean }> = [];
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
    setTimeout: options.observe
      ? (callback: () => void) => { callback(); return 0; }
      : (callback: () => void) => { timers.set(++nextTimer, callback); return nextTimer; },
    clearTimeout: (id: number) => timers.delete(id),
    // 默认不提供 MutationObserver：既有用例验证的是事件确认，不需要等待页面静止。
    ...(options.observe ? { MutationObserver: class {
      entry: { callback: (records: unknown[]) => void; disconnected: boolean };
      constructor(callback: (records: unknown[]) => void) { this.entry = { callback, disconnected: false }; observers.push(this.entry); }
      observe() {}
      disconnect() { this.entry.disconnected = true; }
    } } : {}),
  });
  const prepare = (event = 'click') => runtime.preparePointer(runtime.reference(element), 'pointer', event, null);
  const fire = (trusted: boolean, target = element, event = 'click') => {
    for (const listener of listeners.get(event) ?? []) listener({ isTrusted: trusted, composedPath: () => [target, doc] });
  };
  return { runtime, observers, prepare, fire, timers, listeners, element, rect, scrolls: () => scrolls, cover: () => { covered = true; } };
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

test('点击结果回报文档变化量、目标状态和目标是否仍在文档中，并释放观察者', async () => {
  const f = pointerFixture({ observe: true });
  let expanded = 'false';
  (f.element as unknown as { getAttribute: (name: string) => string | null }).getAttribute =
    (name: string) => name === 'aria-expanded' ? expanded : name === 'aria-label' ? 'Target' : null;
  f.prepare();
  f.fire(true);
  expanded = 'true';
  f.observers[0]!.callback([{}, {}, {}]);
  const result = await f.runtime.finishPointer('pointer', true);
  assert.equal(result.observed, true);
  assert.equal(result.mutations, 3);
  assert.equal(result.connected, true);
  assert.equal(result.stateBefore, 'expanded=false');
  assert.equal(result.stateAfter, 'expanded=true');
  assert.equal(f.observers[0]!.disconnected, true);
});

test('目标在动作后被移出文档会如实回报，用于识别菜单项、弹层按钮这类成功动作', async () => {
  const f = pointerFixture({ observe: true });
  f.prepare();
  f.fire(true);
  (f.element as { isConnected: boolean }).isConnected = false;
  const result = await f.runtime.finishPointer('pointer', true);
  assert.equal(result.connected, false);
  assert.equal(result.stateAfter, '');
});

test('受保护资源的表单提交按钮被结构性拒绝，与按钮文案和语言无关；其他按钮不受影响', () => {
  const cases = [
    { name: '提交按钮 + 密钥对话框', type: 'submit', context: 'Create new secret key\nName\n创建', forbidden: true },
    { name: '中文对话框', type: 'submit', context: '创建新的密钥\n名称', forbidden: true },
    { name: '同一对话框里的取消按钮', type: 'button', context: 'Create new secret key', forbidden: false },
    { name: '无关表单的提交按钮', type: 'submit', context: 'Create tunnel\nName', forbidden: false },
  ];
  for (const item of cases) {
    const f = pointerFixture();
    const dialog = { innerText: item.context, textContent: item.context };
    Object.assign(f.element, {
      tagName: 'BUTTON', form: {},
      getAttribute: (name: string) => name === 'type' ? item.type : name === 'aria-label' ? '确定' : null,
      closest: (selector: string) => selector.includes('dialog') ? dialog : selector.includes('button') ? f.element : null,
    });
    const call = () => f.runtime.preparePointer(f.runtime.reference(f.element), 'pointer', 'click', null, false, ['secret key', '密钥']);
    if (item.forbidden) assert.throws(call, /browser_submit_forbidden/u, item.name);
    else assert.doesNotThrow(call, item.name);
  }
});
