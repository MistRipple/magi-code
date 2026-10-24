import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
import { parse } from 'svelte/compiler';
import { withGoldenViteServer } from './golden-vite.mjs';

// Execute production scroll handlers with only DOM/layout side effects replaced.
const component = await readFile(new URL('../src/components/MessageList.svelte', import.meta.url), 'utf8');
const instance = parse(component).instance;
const script = component.slice(instance.content.start, instance.content.end);
const ast = ts.createSourceFile('MessageList.ts', script, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const handlers = ['handleScroll', 'updateBottomDistance', 'cancelScrollRecoveryForUserIntent', 'compensateLayoutAfterUpdate'].map(name => {
  const declaration = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === name);
  assert.ok(declaration, `missing production handler ${name}`);
  return declaration.getText(ast);
}).join('\n');

await withGoldenViteServer(async server => {
  const { MessageScrollCoordinator, MessageLayoutStabilizer } = await server.ssrLoadModule('/src/lib/message-scroll-coordinator.ts');
  const { deriveMessageScrollDirection } = await server.ssrLoadModule('/src/lib/message-scroll-input.ts');
  const { splitFileName } = await server.ssrLoadModule('/src/lib/file-name.ts');
  const { beginPanelResize } = await server.ssrLoadModule('/src/lib/panel-resize.ts');
  const { imeEnterGuard } = await server.ssrLoadModule('/src/lib/ime-enter-guard.ts');
  const { previewSplitAt, equalPreviewSplitWidth } = await server.ssrLoadModule('/src/web/panel-layout.ts');
  const makeScroll = new Function('MessageScrollCoordinator', 'MessageLayoutStabilizer', 'deriveMessageScrollDirection', ts.transpileModule(`
    let shouldAutoScroll = true, distanceToBottom = 0;
    let lastObservedScrollTop = 500, scrollInteractionEpoch = 0, scrollRecoveryEpoch = 0;
    let activationRestoreNonce = 0, autoScrollScheduleNonce = 0, contentResizeFrame = 0;
    const destroyed = false, isActive = true, layoutObservationNonce = 0;
    const panelKey = 'thread', HISTORY_LOAD_THRESHOLD_PX = 120;
    const containerRef = { scrollTop: 500, scrollHeight: 1000, clientHeight: 500 };
    const scrollCoordinator = new MessageScrollCoordinator();
    const layoutStabilizer = new MessageLayoutStabilizer();
    function currentScrollScopeKey() { return 'session'; }
    function currentHistoryLoadScopeKey() { return 'history'; }
    function updatePanelScrollState(_key, input) { shouldAutoScroll = input.autoScrollEnabled; }
    function cancelScheduledScrollStateSync() {}
    function rememberCurrentLayoutAnchor() {}
    function scheduleScrollStateSync() {}
    function cancelAnimationFrame() {}
    function requestOlderHistoryLoad() {}
    function captureVisibleLayoutAnchor() { return null; }
    function setContainerScrollPosition(top) { containerRef.scrollTop = top; }
    function scrollPanelToBottom() { containerRef.scrollTop = containerRef.scrollHeight - containerRef.clientHeight; }
    ${handlers}
    return {
      metrics: containerRef,
      intent: cancelScrollRecoveryForUserIntent,
      scroll: (top) => { containerRef.scrollTop = top; handleScroll({ target: containerRef }); },
      grow: (px) => { containerRef.scrollHeight += px; compensateLayoutAfterUpdate(0, 'history', scrollInteractionEpoch, scrollRecoveryEpoch); },
      programmatic: (top, intent) => { scrollCoordinator.begin(top, scrollInteractionEpoch, 'session', intent); },
      snapshot: () => ({ follow: shouldAutoScroll, distance: distanceToBottom, top: containerRef.scrollTop }),
    };
  `, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText);
  const harness = () => makeScroll(MessageScrollCoordinator, MessageLayoutStabilizer, deriveMessageScrollDirection);
  const h = harness();
  h.intent();
  assert.equal(h.snapshot().follow, false, 'manual input suspends following before native scroll arrives');
  h.scroll(420);
  h.grow(200);
  assert.deepEqual(h.snapshot(), { follow: false, distance: 280, top: 420 }, 'stream growth leaves the reader in place and updates jump-button distance');
  h.scroll(659);
  assert.equal(h.snapshot().follow, false, '41px from bottom must stay in reading mode');
  h.scroll(658);
  h.scroll(660);
  assert.equal(h.snapshot().follow, true, '40px from bottom resumes following on downward scroll');
  h.scroll(658);
  assert.equal(h.snapshot().follow, false, 'even a small upward scroll suspends following');
  h.programmatic(680, 'restore');
  h.scroll(680);
  assert.equal(h.snapshot().follow, false, 'restoring near bottom cannot turn following back on');
  h.grow(200);
  assert.equal(h.snapshot().top, 680);
  const pending = harness();
  pending.programmatic(500, 'follow-bottom');
  pending.intent();
  pending.scroll(480);
  assert.equal(pending.snapshot().follow, false, 'user input cancels an outstanding follow transaction');

  for (const [name, stem, extension] of [
    ['很长的文件名.pdf', '很长的文件名', '.pdf'],
    ['archive.tar.gz', 'archive', '.tar.gz'],
    ['types.d.ts', 'types', '.d.ts'],
    ['C:\\work\\report.final.PDF', 'C:\\work\\report.final', '.PDF'],
    ['/folder.with.dot/README', '/folder.with.dot/README', ''],
    ['.gitignore', '.gitignore', ''], ['.env.local', '.env', '.local'],
    ['README', 'README', ''], ['file.', 'file.', ''],
  ]) assert.deepEqual(splitFileName(name), { stem, extension });

  const frame = { left: 320, right: 1520, width: 1200 };
  assert.deepEqual(previewSplitAt(920, frame), { width: 596, collapse: false });
  assert.equal(equalPreviewSplitWidth(1200), 596);
  assert.equal(previewSplitAt(1303, frame).collapse, false);
  assert.equal(previewSplitAt(1304, frame).collapse, true, 'collapse threshold excludes the sidebar width');

  class Element extends EventTarget {
    style = { cursor: 'default', userSelect: 'text' };
    captured = null;
    removed = false;
    setAttribute() {}
    setPointerCapture(id) { this.captured = id; }
    hasPointerCapture(id) { return this.captured === id; }
    releasePointerCapture() { this.captured = null; }
    remove() { this.removed = true; }
  }
  const globals = { window: globalThis.window, document: globalThis.document, HTMLElement: globalThis.HTMLElement };
  const body = new Element();
  const shields = [];
  body.appendChild = element => shields.push(element);
  globalThis.HTMLElement = Element;
  globalThis.window = new EventTarget();
  globalThis.document = { body, createElement: () => new Element() };
  const dispatch = (type, fields = {}, target = window) => {
    const event = new Event(type, { cancelable: true });
    Object.assign(event, fields);
    target.dispatchEvent(event);
    return event;
  };
  try {
    // Exercise the production capture guard used by settings, rename and URL fields.
    const input = new Element();
    input.closest = () => input;
    const cleanup = imeEnterGuard(input);
    let submissions = 0;
    const submit = event => { if (event.key === 'Enter') submissions++; };
    input.addEventListener('keydown', submit);
    dispatch('compositionstart', {}, input);
    assert.equal(dispatch('keydown', { key: 'Enter' }, input).defaultPrevented, false,
      'candidate selection keeps its native default action');
    assert.equal(submissions, 0, 'component shortcuts cannot submit while composing');
    dispatch('compositionend', {}, input);
    assert.equal(dispatch('keydown', { key: 'Enter' }, input).defaultPrevented, true,
      'post-composition duplicate Enter is swallowed before nested shortcuts');
    assert.equal(submissions, 0);
    dispatch('keydown', { key: 'Enter', shiftKey: true }, input);
    assert.equal(submissions, 1, 'intentional newline shortcut stays available');
    dispatch('keydown', { key: 'Enter', keyCode: 229 }, input);
    dispatch('keydown', { key: 'Enter', isComposing: true }, input);
    assert.equal(submissions, 1, 'legacy and browser composition flags both protect text fields');
    await new Promise(resolve => setTimeout(resolve, 510));
    dispatch('keydown', { key: 'Enter' }, input);
    assert.equal(submissions, 2, 'ordinary Enter is restored after the grace period');
    const editorChild = new Element();
    editorChild.closest = () => input;
    const composeFromChild = type => {
      const event = new Event(type);
      Object.defineProperty(event, 'target', { value: editorChild });
      input.dispatchEvent(event);
    };
    composeFromChild('compositionstart');
    dispatch('keydown', { key: 'Enter' }, input);
    assert.equal(submissions, 2, 'composition from editor descendants protects the whole input');
    composeFromChild('compositionend');
    assert.equal(dispatch('keydown', { key: 'Enter' }, input).defaultPrevented, true,
      'descendant composition end uses the same editor grace period');
    assert.equal(submissions, 2);
    cleanup.destroy();
    dispatch('compositionstart', {}, input);
    dispatch('keydown', { key: 'Enter' }, input);
    assert.equal(submissions, 3, 'destroy removes all capture listeners');

    for (const ending of ['pointerup', 'pointercancel', 'Escape', 'blur', 'dispose', 'lostcapture']) {
      const handle = new Element();
      const results = [];
      let moves = 0;
      const stop = beginPanelResize({ button: 0, pointerId: 7, clientX: 50, clientY: 10, currentTarget: handle, preventDefault() {} }, {
        move: () => { moves++; }, finish: cancelled => results.push(cancelled),
      });
      assert.equal(shields.at(-1).removed, false);
      assert.equal(body.style.userSelect, 'none');
      dispatch('pointermove', { pointerId: 8, clientX: 100, clientY: 10 });
      dispatch('pointermove', { pointerId: 7, clientX: 52, clientY: 10 });
      assert.equal(moves, 0, 'unrelated pointers and click jitter do not resize');
      dispatch('pointermove', { pointerId: 7, clientX: 100, clientY: 10 });
      assert.equal(moves, 1);
      if (ending === 'Escape') assert.equal(dispatch('keydown', { key: 'Escape' }).defaultPrevented, true);
      else if (ending === 'dispose') stop();
      else if (ending === 'lostcapture') dispatch('lostpointercapture', {}, handle);
      else dispatch(ending, { pointerId: 7 });
      assert.deepEqual(results, [ending !== 'pointerup']);
      assert.equal(shields.at(-1).removed, true, `${ending} removes the shield`);
      assert.equal(handle.captured, null);
      assert.deepEqual(body.style, { cursor: 'default', userSelect: 'text' });
      dispatch('pointermove', { pointerId: 7, clientX: 200, clientY: 10 });
      stop();
      assert.equal(moves, 1, 'listeners stop after transaction cleanup');
      assert.equal(results.length, 1, 'cleanup is idempotent');
      assert.equal(dispatch('click').defaultPrevented, true, 'drag-ending click cannot activate underlying content');
      await new Promise(resolve => setTimeout(resolve, 0));
      assert.equal(dispatch('click').defaultPrevented, false, 'the next deliberate click remains available');
    }
  } finally {
    for (const [name, value] of Object.entries(globals)) {
      if (value === undefined) delete globalThis[name];
      else globalThis[name] = value;
    }
  }
});

console.log('chat interaction golden checks passed (scroll ownership, IME capture, filename suffixes, divider lifecycle)');
