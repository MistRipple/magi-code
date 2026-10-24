import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
import { parse } from 'svelte/compiler';
import { withGoldenViteServer } from './golden-vite.mjs';

// Exercise real handlers without mounting a fake product page.
const modal = await readFile(new URL('../src/components/Modal.svelte', import.meta.url), 'utf8');
const instance = parse(modal).instance;
const script = modal.slice(instance.content.start, instance.content.end);
const ast = ts.createSourceFile('Modal.ts', script, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const handlers = ['handleBackdropClick', 'handleKeydown'].map(name => ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === name).getText(ast)).join('\n');

await withGoldenViteServer(async server => {
  const { loadViewDraft, saveViewDraft, clearViewDraft } = await server.ssrLoadModule('/src/lib/view-drafts.ts');
  const disk = new Map();
  globalThis.sessionStorage = { getItem: key => disk.get(key) ?? null, setItem: (key, value) => disk.set(key, value), removeItem: key => disk.delete(key) };
  const draft = { name: '未保存名称', body: '中文草稿' };
  saveViewDraft('workspace:a:entry:1', 'revision:1', draft);
  assert.deepEqual(loadViewDraft('workspace:a:entry:1', 'revision:1', {}), draft);
  assert.deepEqual(loadViewDraft('workspace:b:entry:1', 'revision:1', {}), {}, 'workspace drafts must be isolated');
  assert.deepEqual(loadViewDraft('workspace:a:entry:1', 'revision:2', {}), {}, 'server changes invalidate stale drafts');
  saveViewDraft('mcp:credential', 'revision:1', { secret: 'test-only' }, 'memory');
  assert.equal([...disk.values()].some(value => value.includes('test-only')), false, 'credential drafts must not enter browser storage');
  assert.deepEqual(loadViewDraft('mcp:credential', 'revision:1', {}, 'memory'), { secret: 'test-only' });
  clearViewDraft('mcp:credential');
  assert.deepEqual(loadViewDraft('mcp:credential', 'revision:1', {}, 'memory'), {});
  globalThis.sessionStorage = { getItem() { throw new Error('blocked'); }, setItem() { throw new Error('blocked'); }, removeItem() {} };
  saveViewDraft('restricted', '1', draft);
  assert.deepEqual(loadViewDraft('restricted', '1', {}), draft, 'blocked session storage retains memory drafts');

  const { topOverlay, registerOverlay, hasOverlay } = await server.ssrLoadModule('/src/lib/overlay-stack.ts');
  const parent = Symbol(), child = Symbol();
  const unregisterParent = registerOverlay(parent), unregisterChild = registerOverlay(child);
  assert.equal(topOverlay(parent), false); assert.equal(topOverlay(child), true);
  unregisterParent(); assert.equal(topOverlay(child), true, 'out-of-order teardown must preserve top layer');
  unregisterChild(); assert.equal(hasOverlay(), false);

  const make = new Function('topOverlay', ts.transpileModule(`
    return (form, isTop = true, escapeEnabled = true) => {
      const overlayEl = {}, layer = isTop, closeOnBackdrop = true, closeOnEscape = escapeEnabled;
      let closes = 0;
      const onClose = () => closes++;
      const dialogEl = undefined;
      ${handlers}
      return { backdrop: () => handleBackdropClick({target: overlayEl}), key: handleKeydown, closes: () => closes };
    };
  `, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText)(value => value);
  const event = () => ({ key: 'Escape', preventDefault() { this.prevented = true; }, stopImmediatePropagation() { this.stopped = true; } });
  const form = make(true); form.backdrop(); assert.equal(form.closes(), 0);
  form.key(event()); assert.equal(form.closes(), 1);
  const confirmation = make(false); confirmation.backdrop(); assert.equal(confirmation.closes(), 1);
  const underlying = make(false, false); underlying.key(event()); assert.equal(underlying.closes(), 0);
  const saving = make(true, true, false); const escape = event(); saving.key(escape);
  assert.equal(saving.closes(), 0); assert.equal(escape.prevented, true); assert.equal(escape.stopped, true, 'busy form must not let Escape dismiss parent settings');
});
console.log('overlay interaction golden passed');
