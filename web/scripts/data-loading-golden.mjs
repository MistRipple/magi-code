import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = Object.assign(value => value, { snapshot: value => value, raw: value => value });
globalThis.$derived = Object.assign(value => value, { by: fn => fn() });
globalThis.$effect = Object.assign(() => {}, { root: fn => { fn(); return () => {}; } });
const events = new EventTarget();
const intervals = new Map();
const timeouts = new Map();
let timerId = 0;
const storage = { getItem() { return null; }, setItem() {}, removeItem() {} };
globalThis.window = {
  location: { href: 'http://127.0.0.1:38123/web.html', origin: 'http://127.0.0.1:38123' },
  localStorage: storage,
  setTimeout(fn, ms) {
    const id = ++timerId;
    timeouts.set(id, { fn: () => { timeouts.delete(id); fn(); }, ms });
    return id;
  },
  clearTimeout(id) { timeouts.delete(id); },
  setInterval(fn, ms) { const id = ++timerId; intervals.set(id, { fn, ms }); return id; },
  clearInterval(id) { intervals.delete(id); },
  addEventListener: events.addEventListener.bind(events),
  removeEventListener: events.removeEventListener.bind(events),
  dispatchEvent: events.dispatchEvent.bind(events),
};
globalThis.localStorage = storage;
const json = (value, status = 200) => new Response(JSON.stringify(value), {
  status, headers: { 'content-type': 'application/json' },
});
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
const pending = new Map();
const counts = new Map();
globalThis.fetch = async (url) => {
  const path = new URL(String(url)).pathname;
  counts.set(path, (counts.get(path) ?? 0) + 1);
  const next = pending.get(path)?.shift();
  if (next) return next();
  return json({ message: `unexpected request ${path}` }, 404);
};
const queue = (path, ...responses) => pending.set(path, [...(pending.get(path) ?? []), ...responses]);
const tick = () => new Promise(r => setTimeout(r, 0));

await withGoldenViteServer(async server => {
  const external = await server.ssrLoadModule('/src/stores/external-sessions.svelte.ts');
  const path = '/api/mcp-server/sessions';
  queue(path, () => json({ sessions: [{ sessionId: 'old' }] }), () => { throw new TypeError('offline'); });
  await external.refreshExternalSessions();
  await external.refreshExternalSessions();
  assert.equal(external.externalSessions.sessions[0].sessionId, 'old', '临时失败不能清空外部会话');
  queue(path, () => json({ sessions: [{ sessionId: 'retried' }] }));
  timeouts.values().find(item => item.ms === 3000).fn();
  await tick();
  assert.equal(external.externalSessions.sessions[0].sessionId, 'retried', '临时失败无需点击或新事件就能补读');
  const slow = deferred();
  queue(path, () => slow.promise, () => json({ sessions: [{ sessionId: 'new' }] }));
  const stale = external.refreshExternalSessions();
  await external.refreshExternalSessions();
  slow.resolve(json({ sessions: [{ sessionId: 'stale' }] }));
  await stale;
  assert.equal(external.externalSessions.sessions[0].sessionId, 'new', '晚到的外部会话快照不能覆盖新结果');
  queue(path, () => json({ message: 'forbidden' }, 403));
  await external.refreshExternalSessions();
  assert.deepEqual(external.externalSessions.sessions, [], '明确无权限才清空入口');

  const approvals = await server.ssrLoadModule('/src/stores/external-approvals.svelte.ts');
  const approvalsPath = '/api/mcp-server/approvals';
  const approval = { approvalId: 'approval-1', clientName: 'fixture', summary: 'read' };
  queue(approvalsPath, () => { throw new TypeError('offline'); }, () => json({ approvals: [approval] }));
  const stop = approvals.trackExternalApprovals();
  await tick();
  intervals.values().find(item => item.ms === 3000).fn();
  await tick();
  assert.equal(approvals.externalApprovals.approvals.length, 1, '首次网络失败后无需新事件就能重试');
  queue(approvalsPath, () => json({ message: 'unavailable' }, 503));
  await approvals.refreshExternalApprovals();
  assert.equal(approvals.externalApprovals.approvals.length, 1, '503 保留待确认并继续跟踪');
  const staleApprovals = deferred();
  queue(approvalsPath, () => staleApprovals.promise);
  const staleRead = approvals.refreshExternalApprovals();
  const readsBeforeResolve = counts.get(approvalsPath);
  queue('/api/mcp-server/approvals/resolve', () => json({ approvals: [] }));
  await approvals.resolveExternalApproval(approval, 'allow_once');
  assert.equal(counts.get(approvalsPath), readsBeforeResolve, '处理成功直接消费返回快照，无变化事件时不能重复读取');
  staleApprovals.resolve(json({ approvals: [approval] }));
  await staleRead;
  assert.deepEqual(approvals.externalApprovals.approvals, [], '批准前发出的读取不能复活已处理审批');
  const resolving = deferred();
  queue('/api/mcp-server/approvals/resolve', () => resolving.promise);
  queue(approvalsPath, () => json({ approvals: [approval] }));
  const resolveWithEvent = approvals.resolveExternalApproval(approval, 'allow_once');
  window.dispatchEvent(new Event('magi:mcp-approvals-changed'));
  resolving.resolve(json({ approvals: [] }));
  await resolveWithEvent;
  assert.equal(approvals.externalApprovals.approvals.length, 1, '处理期间收到新审批事件必须补读，不能被 busy 丢弃');
  queue(approvalsPath, () => json({ message: 'forbidden' }, 403));
  await approvals.refreshExternalApprovals();
  const count = counts.get(approvalsPath);
  intervals.values().find(item => item.ms === 3000).fn();
  await tick();
  assert.equal(counts.get(approvalsPath), count, '无权限时停止轮询');
  queue(approvalsPath, () => json({ approvals: [approval] }));
  window.dispatchEvent(new CustomEvent('magi-runtime-connection', { detail: { status: 'connected' } }));
  await tick();
  assert.equal(approvals.externalApprovals.approvals.length, 1, '重连后重新核对权限及待确认');
  stop();

  const registry = await server.ssrLoadModule('/src/stores/plugin-registry.svelte.ts');
  const plugin = { id: 'fixture.plugin', version: '1', contributions: {} };
  const manifestsPath = '/api/plugins/manifests';
  const pluginResponse = manifestResponse => {
    queue(manifestsPath, manifestResponse);
    queue('/api/plugins/commands', () => json([]));
    queue('/api/plugins/workflows', () => json([]));
  };
  let invalidations = 0;
  window.addEventListener('magi:pluginSelectionInvalidated', () => invalidations++);
  pluginResponse(() => json([plugin]));
  await registry.loadedPluginProjection('application');
  window.dispatchEvent(new Event('magi-plugin-registry-changed'));
  pluginResponse(() => json({ message: 'temporary' }, 503));
  const failed = await registry.loadedPluginProjection('application');
  assert.equal(failed.manifests[0].id, plugin.id);
  assert.equal(invalidations, 0, '临时读取失败不能撤销插件选择或关闭页面');
  assert.equal(failed.loaded, false, '旧清单仍可展示，但读取失败后不能据其缺项撤销选择');
  pluginResponse(() => json([plugin]));
  assert.equal((await registry.loadedPluginProjection('application')).loaded, true, '失败读取允许重试');
  pluginResponse(() => json({ message: 'offline' }, 503));
  assert.equal((await registry.loadedPluginProjection('initial-failure')).loaded, false, '首次失败不能显示为已加载空列表');
  const oldPlugins = deferred();
  window.dispatchEvent(new Event('magi-plugin-registry-changed'));
  pluginResponse(() => oldPlugins.promise);
  const oldLoad = registry.loadedPluginProjection('application');
  window.dispatchEvent(new Event('magi-plugin-registry-changed'));
  pluginResponse(() => json([]));
  await registry.loadedPluginProjection('application');
  oldPlugins.resolve(json([plugin]));
  const settled = await oldLoad;
  assert.deepEqual(settled.manifests, [], '旧注册表请求不能恢复已停用插件');
  assert.equal(invalidations, 1, '成功读取到插件确实被移除后才撤销选择');
  const invalidatedPlugins = deferred();
  window.dispatchEvent(new Event('magi-plugin-registry-changed'));
  pluginResponse(() => invalidatedPlugins.promise);
  const invalidatedLoad = registry.loadedPluginProjection('application');
  window.dispatchEvent(new Event('magi-plugin-registry-changed'));
  pluginResponse(() => json([plugin]));
  invalidatedPlugins.resolve(json([]));
  assert.equal((await invalidatedLoad).manifests[0].id, plugin.id, '等待者必须主动补读失效后的修订，不依赖另一个消费者');

  const waits = await server.ssrLoadModule('/src/stores/workspace-wait-store.svelte.ts');
  const oldWaits = deferred();
  queue('/api/session/workspace-waits', () => oldWaits.promise);
  const waitingRead = waits.syncWorkspaceWaits();
  waits.clearWorkspaceWaiting('ready-session');
  oldWaits.resolve(json({ waits: [
    { sessionId: 'ready-session', blockingSessionIds: ['other'] },
    { sessionId: 'waiting-session', blockingSessionIds: ['blocker'] },
  ] }));
  await waitingRead;
  assert.equal(waits.getWorkspaceWait('ready-session'), null, '旧快照不能撤销 workspace.ready');
  assert.deepEqual(waits.getWorkspaceWait('waiting-session'), { blockingSessionIds: ['blocker'] }, '单个会话事件不能导致其他会话的快照丢失');

  const questions = await server.ssrLoadModule('/src/stores/user-question-store.svelte.ts');
  const questionsPath = '/api/session/user-questions';
  queue(questionsPath, () => json({ pendingQuestions: [{ questionId: 'q-old' }] }));
  await questions.syncUserQuestions('old-session');
  const newQuestions = deferred();
  queue(questionsPath, () => newQuestions.promise);
  const changed = questions.syncUserQuestions('new-session');
  assert.equal(questions.userQuestionState.hydrated, false);
  assert.deepEqual(questions.userQuestionState.pending, [], '换会话不能沿用上一会话的待回答');
  assert.equal(questions.isUserQuestionPending('new-session', 'q-new'), true, '新作用域未就绪不能认定问题已结束');
  newQuestions.resolve(json({ pendingQuestions: [{ questionId: 'q-new' }] }));
  await changed;
  const toolApprovals = await server.ssrLoadModule('/src/stores/tool-approval-store.svelte.ts');
  queue('/api/session/tool-approvals', () => json({ pendingApprovals: [{ approvalId: 'old' }] }));
  await toolApprovals.syncToolApprovals('old-session');
  const newTools = deferred();
  queue('/api/session/tool-approvals', () => newTools.promise);
  const toolChanged = toolApprovals.syncToolApprovals('new-session');
  assert.deepEqual(toolApprovals.toolApprovalState.pending, [], '换会话不能沿用上一会话的审批');
  newTools.resolve(json({ pendingApprovals: [] }));
  await toolChanged;

  const goals = await server.ssrLoadModule('/src/stores/goal-store.svelte.ts');
  const goalPath = '/api/goals/current';
  const plan = { planId: 'plan-1', revision: 1, items: [], taskBindings: {}, taskStatuses: {} };
  queue(goalPath, () => json({ goal: null, plan }));
  await goals.refreshCurrentGoal('plan-session', '', '');
  const slowGoal = deferred();
  queue(goalPath, () => slowGoal.promise);
  const goalRead = goals.refreshCurrentGoal('plan-session', '', '');
  goals.applySessionPlanSnapshot('plan-session', '', { ...plan, revision: 2 });
  slowGoal.resolve(json({ goal: null, plan }));
  await goalRead;
  assert.equal(goals.getGoalState('plan-session', '').response.plan.revision, 2, '已有计划刷新期间的实时更新不能被旧 HTTP 快照覆盖');
});
console.log('data loading recovery and race checks passed');
