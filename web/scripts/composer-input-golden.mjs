import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
import { parse } from 'svelte/compiler';

// 执行组件中的真实事件处理函数；只替换发送、换行和菜单提交等副作用。
// 这验证组合事件合同，不替代操作系统候选窗的真实验收。
const component = await readFile(new URL('../src/components/InputArea.svelte', import.meta.url), 'utf8');
const instance = parse(component).instance;
assert.ok(instance, 'InputArea must expose its component script');
const script = component.slice(instance.content.start, instance.content.end);
const ast = ts.createSourceFile('InputArea.ts', script, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const names = ['handleKeydown', 'isEnterKey', 'isNewlineShortcut', 'insertNewlineAtCursor', 'handleCompositionStart', 'handleCompositionEnd'];
const handlers = names.map(name => {
  const declaration = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === name);
  assert.ok(declaration, `missing production handler ${name}`);
  return declaration.getText(ast);
}).join('\n');
const makeHarness = new Function(ts.transpileModule(`
  let isComposing = false;
  let compositionEndedAt = 0;
  let now = 1000;
  const Date = { now: () => now };
  let slashMenuOpen = false;
  let slashHighlightIndex = 0;
  const filteredSlashCommands = [{ id: 'first' }, { id: 'second' }];
  const calls: string[] = [];
  function sendMessage() { calls.push('send'); }
  function invalidateEnhanceState() {}
  function insertEditorText(text: string) { calls.push('insert:' + text); }
  function handleComposerInput() { calls.push('input'); }
  function closeSlashMenu() { slashMenuOpen = false; calls.push('close'); }
  function commitSlashCommand(command: { id: string }) { calls.push('commit:' + command.id); }
  ${handlers}
  return {
    keydown: handleKeydown,
    start: handleCompositionStart,
    end: handleCompositionEnd,
    menu: () => { slashMenuOpen = true; },
    advance: (ms) => { now += ms; },
    snapshot: () => ({ calls: [...calls], index: slashHighlightIndex, menuOpen: slashMenuOpen }),
  };
`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText);

let scenarios = 0;
function press(harness, overrides = {}) {
  const event = {
    key: 'Enter', code: 'Enter', keyCode: 13,
    isComposing: false, shiftKey: false, altKey: false, metaKey: false, ctrlKey: false,
    prevented: false,
    preventDefault() { this.prevented = true; },
    ...overrides,
  };
  harness.keydown(event);
  return event;
}
function check(label, run) {
  run();
  scenarios += 1;
}

for (const [label, keys, expected, prevented] of [
  ['Enter sends', {}, ['send'], true],
  ['NumpadEnter sends', { key: '', code: 'NumpadEnter' }, ['send'], true],
  ['Shift+Enter inserts a newline', { shiftKey: true }, ['close', 'insert:\n'], true],
  ['Alt+Enter inserts a newline', { altKey: true }, ['close', 'insert:\n'], true],
  ['Cmd+Enter does not accidentally send', { metaKey: true }, [], true],
  ['Ctrl+Enter does not accidentally send', { ctrlKey: true }, [], true],
  ['unrelated key stays native', { key: 'a', code: 'KeyA', keyCode: 65 }, [], false],
]) {
  check(label, () => {
    const h = makeHarness();
    const e = press(h, keys);
    assert.deepEqual(h.snapshot().calls, expected, label);
    assert.equal(e.prevented, prevented, label);
  });
}

for (const signal of ['event flag', 'legacy 229', 'component composition']) {
  for (const menuOpen of [false, true]) {
    for (const keys of [
      {}, { shiftKey: true }, { altKey: true },
      { key: 'Tab', code: 'Tab', keyCode: 9 },
      { key: 'ArrowDown', code: 'ArrowDown', keyCode: 40 },
      { key: 'ArrowUp', code: 'ArrowUp', keyCode: 38 },
      { key: 'Escape', code: 'Escape', keyCode: 27 },
    ]) {
      check(`${signal} preserves IME ${keys.key ?? 'Enter'} (menu=${menuOpen})`, () => {
        const h = makeHarness();
        if (menuOpen) h.menu();
        const eventSignal = signal === 'event flag' ? { isComposing: true }
          : signal === 'legacy 229' ? { keyCode: 229 } : {};
        if (signal === 'component composition') h.start();
        const e = press(h, { ...keys, ...eventSignal });
        assert.deepEqual(h.snapshot(), { calls: [], index: 0, menuOpen }, `${signal}: must leave candidate selection to the IME`);
        assert.equal(e.prevented, false, `${signal}: must not swallow candidate confirmation`);
      });
    }
  }
}

check('composition ending suppresses Safari duplicate Enter, later ordinary Enter submits once', () => {
  const h = makeHarness();
  h.start();
  press(h, { isComposing: true });
  h.end();
  press(h, { keyCode: 229 });
  assert.deepEqual(h.snapshot().calls, ['input'], 'IME completion only synchronizes text');
  press(h);
  assert.deepEqual(h.snapshot().calls, ['input'], 'duplicate Enter inside the composition grace period is swallowed');
  h.advance(501);
  press(h);
  assert.deepEqual(h.snapshot().calls, ['input', 'send']);
});

for (const menuOpen of [false, true]) {
  check(`composition grace period blocks duplicate command selection (menu=${menuOpen})`, () => {
    const h = makeHarness();
    h.start();
    h.end();
    if (menuOpen) h.menu();
    h.advance(499);
    assert.equal(press(h).prevented, true);
    assert.deepEqual(h.snapshot().calls, ['input']);
    h.advance(1);
    press(h);
    assert.deepEqual(h.snapshot().calls, ['input', menuOpen ? 'commit:first' : 'send']);
  });
}

check('intentional newline remains available immediately after candidate confirmation', () => {
  const h = makeHarness();
  h.start();
  h.end();
  press(h, { shiftKey: true });
  assert.deepEqual(h.snapshot().calls, ['input', 'close', 'insert:\n']);
});

for (const [label, keys, expected] of [
  ['Enter selects a command', {}, ['commit:first']],
  ['Tab selects a command', { key: 'Tab', code: 'Tab', keyCode: 9 }, ['commit:first']],
  ['Escape closes the menu', { key: 'Escape', code: 'Escape', keyCode: 27 }, ['close']],
  ['Shift+Enter adds a newline instead of selecting a command', { shiftKey: true }, ['close', 'insert:\n']],
  ['Alt+Enter adds a newline instead of selecting a command', { altKey: true }, ['close', 'insert:\n']],
]) {
  check(label, () => {
    const h = makeHarness();
    h.menu();
    assert.equal(press(h, keys).prevented, true);
    assert.deepEqual(h.snapshot().calls, expected, label);
  });
}
check('menu navigation wraps without sending', () => {
  const h = makeHarness();
  h.menu();
  press(h, { key: 'ArrowUp', code: 'ArrowUp' });
  assert.equal(h.snapshot().index, 1);
  press(h, { key: 'ArrowDown', code: 'ArrowDown' });
  assert.equal(h.snapshot().index, 0);
  assert.deepEqual(h.snapshot().calls, []);
});

console.log(`composer input golden checks passed (${scenarios} scenarios)`);
