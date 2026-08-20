import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

const WORKSPACE_ID = 'workspace-session-suggestions-golden';
const WORKSPACE_PATH = '/tmp/workspace-session-suggestions-golden';
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

function group(prefix) {
  return {
    suggestions: [
      { category: 'understand', label: `${prefix}-理解`, prompt: `${prefix}-了解项目结构` },
      { category: 'plan', label: `${prefix}-计划`, prompt: `${prefix}-制定下一步计划` },
      { category: 'execute', label: `${prefix}-执行`, prompt: `${prefix}-推进当前任务` },
    ],
  };
}

const requests = [];
let responder = null;
globalThis.fetch = async (url, init) => {
  const parsed = new URL(String(url));
  const body = init?.body ? JSON.parse(init.body) : {};
  requests.push({ pathname: parsed.pathname, body });
  return responder(requests.length, body, init?.signal);
};

function jsonResponse(payload) {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

async function waitFor(predicate, label) {
  const deadline = Date.now() + 2000;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(label);
}

await withGoldenViteServer(async (server) => {
  const { sessionSuggestions } = await server.ssrLoadModule('/src/stores/session-suggestions.svelte.ts');
  const scope = {
    key: `${WORKSPACE_ID}:zh-CN:v1`,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
    locale: 'zh-CN',
  };

  // 首次生成：请求两组，激活第一组并把第二组留作备用。
  responder = () => jsonResponse({ groups: [group('a'), group('b')] });
  sessionSuggestions.ensure(scope);
  assert.equal(sessionSuggestions.loadingInitial, true, '首次生成期间必须暴露骨架加载态');
  assert.equal(sessionSuggestions.activeGroup, null, '骨架期间不得渲染任何建议');
  await waitFor(() => !sessionSuggestions.generating, '首次生成必须结束');

  assert.equal(requests.length, 1, '首次生成只能发起一次请求');
  assert.equal(requests[0].pathname, '/api/prompt/suggestions');
  assert.equal(requests[0].body.requestedGroups, 2, '首次必须一次取回激活组与备用组');
  assert.equal(requests[0].body.count, 3);
  assert.equal(requests[0].body.sessionId, SESSION_ID, '必须透传 sessionId 以记录辅助模型用量');
  assert.equal(requests[0].body.workspaceId, WORKSPACE_ID);
  assert.equal(requests[0].body.workspacePath, WORKSPACE_PATH);
  assert.deepEqual(requests[0].body.excludePrompts, [], '首次生成没有需要排除的建议');
  assert.equal(sessionSuggestions.loadingInitial, false);
  assert.equal(sessionSuggestions.unavailable, false);
  assert.deepEqual(
    sessionSuggestions.activeGroup.suggestions.map((item) => item.prompt),
    ['a-了解项目结构', 'a-制定下一步计划', 'a-推进当前任务'],
  );
  assert.equal(sessionSuggestions.standbyGroup.suggestions[0].prompt, 'b-了解项目结构');

  // 换一组：立即切到备用组，并在后台补齐下一组，排除已展示的建议。
  responder = () => jsonResponse({ groups: [group('c')] });
  sessionSuggestions.rotate(scope);
  assert.equal(sessionSuggestions.activeGroup.suggestions[0].prompt, 'b-了解项目结构', '备用组必须立即生效');
  await waitFor(() => !sessionSuggestions.generating, '补充生成必须结束');
  assert.equal(requests.length, 2);
  assert.equal(requests[1].body.requestedGroups, 1, '换一组只需补充一组');
  assert.deepEqual(
    requests[1].body.excludePrompts,
    ['b-了解项目结构', 'b-制定下一步计划', 'b-推进当前任务'],
    '补充生成必须排除当前展示的建议',
  );
  assert.equal(sessionSuggestions.standbyGroup.suggestions[0].prompt, 'c-了解项目结构');

  // 后端拒绝时保留当前建议，不回落到任何前端硬编码文案。
  responder = () => new Response(JSON.stringify({ message: '辅助模型未配置' }), {
    status: 400,
    headers: { 'content-type': 'application/json' },
  });
  const beforeFailure = sessionSuggestions.activeGroup.suggestions[0].prompt;
  sessionSuggestions.rotate(scope);
  await waitFor(() => !sessionSuggestions.generating, '失败请求必须收敛');
  assert.equal(sessionSuggestions.activeGroup.suggestions[0].prompt, 'c-了解项目结构');
  assert.notEqual(beforeFailure, undefined);
  assert.equal(sessionSuggestions.unavailable, false, '已有建议时失败不得进入不可用态');

  // 个人域：没有工作区绑定时不得携带 workspace 字段。
  const personalScope = {
    key: 'personal:zh-CN:v1',
    workspaceId: '',
    workspacePath: '',
    sessionId: '',
    locale: 'zh-CN',
  };
  responder = () => jsonResponse({ groups: [group('p1'), group('p2')] });
  sessionSuggestions.ensure(personalScope);
  await waitFor(() => !sessionSuggestions.generating, '个人域生成必须结束');
  const personalRequest = requests.at(-1);
  assert.equal(personalRequest.body.workspaceId, undefined, '个人域不得携带 workspaceId');
  assert.equal(personalRequest.body.workspacePath, undefined, '个人域不得携带 workspacePath');
  assert.equal(personalRequest.body.sessionId, undefined, '草稿会话不得携带空 sessionId');

  // 首次生成失败必须进入显式不可用态，而不是静默展示兜底文案。
  const failingScope = {
    key: 'failing:zh-CN:v1',
    workspaceId: 'workspace-failing',
    workspacePath: '/tmp/workspace-failing',
    sessionId: SESSION_ID,
    locale: 'zh-CN',
  };
  responder = () => new Response('boom', { status: 500 });
  sessionSuggestions.ensure(failingScope);
  await waitFor(() => !sessionSuggestions.generating, '失败的首次生成必须结束');
  assert.equal(sessionSuggestions.activeGroup, null, '失败时不得使用前端硬编码建议');
  assert.equal(sessionSuggestions.unavailable, true, '失败必须暴露可见的不可用态');
  assert.equal(sessionSuggestions.loadingInitial, false);

  // 作用域切回已缓存条目时直接复用，不再重复请求辅助模型。
  const requestsBeforeRestore = requests.length;
  sessionSuggestions.ensure(scope);
  assert.equal(requests.length, requestsBeforeRestore, '已缓存作用域不得重复生成');
  assert.equal(sessionSuggestions.activeGroup.suggestions[0].prompt, 'c-了解项目结构');

  console.log('session suggestions golden passed');
});
