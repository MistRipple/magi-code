import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

const WORKSPACE_ID = 'workspace-session-suggestions-golden';
const SESSION_ID = 'session-suggestions-golden';

globalThis.window = {
  location: { href: 'http://127.0.0.1:38123/web.html' },
  localStorage: {
    getItem() { return null; },
    setItem() {},
    removeItem() {},
  },
};
globalThis.localStorage = globalThis.window.localStorage;

const requests = [];
globalThis.fetch = async (url, init) => {
  const parsed = new URL(String(url));
  const body = init?.body ? JSON.parse(init.body) : {};
  requests.push({ pathname: parsed.pathname, body });
  throw new Error(`built-in suggestions must not fetch: ${parsed.pathname}`);
};

function promptSet(group) {
  return new Set(group.suggestions.map((suggestion) => suggestion.prompt));
}

function categorySet(group) {
  return new Set(group.suggestions.map((suggestion) => suggestion.category));
}

await withGoldenViteServer(async (server) => {
  const {
    BUILT_IN_SUGGESTION_COUNT,
    sessionSuggestions,
  } = await server.ssrLoadModule('/src/stores/session-suggestions.svelte.ts');
  const scope = {
    key: `${WORKSPACE_ID}:${SESSION_ID}:zh-CN:builtin-v1`,
    locale: 'zh-CN',
  };

  assert.equal(BUILT_IN_SUGGESTION_COUNT, 30, '内置建议池必须包含 30 条选项');

  // 首次进入：同步从内置池抽取 3 条不同类别的建议，不依赖网络或 auxiliary 模型。
  sessionSuggestions.ensure(scope);
  assert.equal(requests.length, 0, '首次展示不得发起网络请求');
  assert.equal(sessionSuggestions.activeGroup.suggestions.length, 3);
  assert.equal(promptSet(sessionSuggestions.activeGroup).size, 3, '同一组建议不得重复');
  assert.equal(categorySet(sessionSuggestions.activeGroup).size, 3, '同一组建议应覆盖不同类别');
  assert.ok(sessionSuggestions.activeGroup.suggestions.every((item) => (
    item.label
    && item.prompt
    && !item.label.startsWith('messageList.suggestions.items.')
    && !item.prompt.startsWith('messageList.suggestions.items.')
  )));

  // 换一组：同步重新抽取 3 条不同类别的建议，并排除当前展示的建议。
  const firstGroup = sessionSuggestions.activeGroup;
  sessionSuggestions.rotate(scope);
  assert.equal(requests.length, 0, '换一组不得发起网络请求');
  assert.equal(sessionSuggestions.activeGroup.suggestions.length, 3);
  assert.equal(promptSet(sessionSuggestions.activeGroup).size, 3, '换组后建议不得重复');
  assert.equal(categorySet(sessionSuggestions.activeGroup).size, 3, '换组后建议应覆盖不同类别');
  assert.equal(
    [...promptSet(sessionSuggestions.activeGroup)].some((prompt) => promptSet(firstGroup).has(prompt)),
    false,
    '换组后不得复用当前展示的词条',
  );

  // 作用域切回时复用当前作用域的本地抽样结果。
  const currentGroup = sessionSuggestions.activeGroup;
  sessionSuggestions.ensure(scope);
  assert.equal(requests.length, 0, '恢复缓存作用域不得发起网络请求');
  assert.deepEqual(sessionSuggestions.activeGroup, currentGroup);

  // 个人域也直接使用相同的内置建议池。
  const personalScope = {
    key: 'personal:draft:zh-CN:builtin-v1',
    locale: 'zh-CN',
  };
  sessionSuggestions.ensure(personalScope);
  assert.equal(requests.length, 0, '个人域不得发起网络请求');
  assert.equal(sessionSuggestions.activeGroup.suggestions.length, 3);

  console.log('session suggestions built-in golden passed');
});
