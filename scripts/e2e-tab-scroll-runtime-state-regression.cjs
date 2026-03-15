#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function installBrowserEnv() {
  global.$state = (value) => value;
  global.$derived = Object.assign((value) => value, {
    by: (factory) => factory(),
  });

  const storage = new Map();
  global.localStorage = {
    getItem(key) { return storage.has(key) ? storage.get(key) : null; },
    setItem(key, value) { storage.set(key, String(value)); },
    removeItem(key) { storage.delete(key); },
    clear() { storage.clear(); },
  };

  global.window = {
    __INITIAL_LOCALE__: 'zh-CN',
    __DEBUG_MODE__: false,
    addEventListener() {},
    removeEventListener() {},
    localStorage: global.localStorage,
  };
}

function ensureCompiled(file) {
  if (!fs.existsSync(file)) {
    throw new Error(`缺少 out 编译产物: ${file}，请先执行 npm run -s compile`);
  }
}

function writePersistedState(state) {
  global.localStorage.setItem('webview-state', JSON.stringify(state));
}

async function main() {
  installBrowserEnv();
  const sourcePath = path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.ts');
  const storePath = path.join(OUT, 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');
  ensureCompiled(storePath);

  const source = fs.readFileSync(sourcePath, 'utf8');
  assert(source.includes('function resetPanelScrollRuntimeState(): void {'), '缺少面板滚动运行态重置入口');
  assert(!source.includes('scrollPositions: messagesState.scrollPositions,'), '滚动位置仍被持久化');
  assert(source.includes('resetPanelScrollRuntimeState();\n  const persisted = vscode.getState<WebviewPersistedState>();'), 'initializeState 未先重置滚动运行态');
  assert(source.includes('if (hasChanged) {\n    resetPanelScrollRuntimeState();\n  }'), '会话切换未重置滚动运行态');

  writePersistedState({
    currentTopTab: 'thread',
    currentBottomTab: 'thread',
    threadMessages: [],
    agentOutputs: { claude: [], codex: [], gemini: [] },
    sessions: [],
    currentSessionId: 'session-a',
    scrollPositions: { thread: 188, claude: 77, codex: 66, gemini: 55 },
    scrollAnchors: {
      thread: { messageId: 'old-thread', offsetTop: 14 },
      claude: { messageId: 'old-claude', offsetTop: 21 },
      codex: { messageId: 'old-codex', offsetTop: 22 },
      gemini: { messageId: 'old-gemini', offsetTop: 23 },
    },
    autoScrollEnabled: { thread: false, claude: false, codex: false, gemini: false },
    notificationBuckets: {},
    orchestratorRuntimeDiagnostics: null,
  });

  const store = require(storePath);
  const {
    messagesState,
    initializeState,
    setCurrentSessionId,
    updatePanelScrollState,
  } = store;

  initializeState();

  assert(messagesState.scrollPositions.thread === 0, `初始化后 thread scrollTop 应重置为 0，实际 ${messagesState.scrollPositions.thread}`);
  assert(messagesState.scrollPositions.claude === 0, `初始化后 claude scrollTop 应重置为 0，实际 ${messagesState.scrollPositions.claude}`);
  assert(messagesState.autoScrollEnabled.thread === true, '初始化后 thread 应默认锁底');
  assert(messagesState.autoScrollEnabled.claude === true, '初始化后 claude 应默认锁底');
  assert(messagesState.scrollAnchors.thread.messageId === null, '初始化后 thread anchor 应清空');

  updatePanelScrollState('thread', {
    scrollTop: 321,
    autoScrollEnabled: false,
    anchor: { messageId: 'msg-thread', offsetTop: 33 },
  }, { persist: false });
  updatePanelScrollState('claude', {
    scrollTop: 123,
    autoScrollEnabled: false,
    anchor: { messageId: 'msg-claude', offsetTop: 18 },
  }, { persist: false });

  setCurrentSessionId('session-b');

  assert(messagesState.scrollPositions.thread === 0, `会话切换后 thread scrollTop 应重置为 0，实际 ${messagesState.scrollPositions.thread}`);
  assert(messagesState.scrollPositions.claude === 0, `会话切换后 claude scrollTop 应重置为 0，实际 ${messagesState.scrollPositions.claude}`);
  assert(messagesState.autoScrollEnabled.thread === true, '会话切换后 thread 应恢复默认锁底');
  assert(messagesState.autoScrollEnabled.claude === true, '会话切换后 claude 应恢复默认锁底');
  assert(messagesState.scrollAnchors.thread.messageId === null, '会话切换后 thread anchor 应清空');
  assert(messagesState.scrollAnchors.claude.messageId === null, '会话切换后 claude anchor 应清空');

  console.log('\n=== tab scroll runtime state regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'scroll_state_not_restored_from_persisted_webview_state',
      'session_switch_resets_panel_scroll_runtime_state',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('tab scroll runtime state 回归失败:', error?.stack || error);
  process.exit(1);
});
