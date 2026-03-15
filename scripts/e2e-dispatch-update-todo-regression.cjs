#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const Module = require('module');
const { EventEmitter } = require('events');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');
const originalLoad = Module._load;
Module._load = function patchedLoad(request, parent, isMain) {
  if (request === 'vscode') return {};
  return originalLoad.call(this, request, parent, isMain);
};

function assert(condition, message) { if (!condition) throw new Error(message); }
function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  return require(abs);
}

function testSourceGuardrails() {
  const source = fs.readFileSync(path.join(ROOT, 'src', 'orchestrator', 'core', 'dispatch-manager.ts'), 'utf8');
  assert(source.includes("await todoManager.resetToPending(update.todoId, { force: forceReset });"), 'todo_update 未委托 TodoManager.resetToPending');
  assert(source.includes('await todoManager.skip(update.todoId);'), 'todo_update 未委托 TodoManager.skip');
  assert(!source.includes('pendingAllowedSource'), '仍残留 pendingAllowedSource 第二状态机');
  assert(!source.includes('skippedAllowedSource'), '仍残留 skippedAllowedSource 第二状态机');
}

function createHarness(DispatchManager, todoManager) {
  let handlers;
  const interrupts = [];
  const orchestrationExecutor = { setAvailableWorkers() {}, setCategoryWorkerMap() {}, setHandlers(next) { handlers = next; } };
  const manager = new DispatchManager({
    adapterFactory: { interrupt(worker) { interrupts.push(worker); return Promise.resolve(); }, getToolManager() { return { getOrchestrationExecutor() { return orchestrationExecutor; }, refreshToolSchemas() {} }; } },
    profileLoader: { getEnabledProfiles() { return new Map([['codex', { worker: 'codex', persona: { strengths: ['code', 'fix'] } }]]); }, getAllCategories() { return new Map([['general', { displayName: 'General' }]]); }, getAssignmentLoader() { return { getCategoryMap() { return { general: 'codex' }; }, reload() {} }; }, getWorkerForCategory() { return 'codex'; }, getCategory() { return { name: 'general' }; } },
    messageHub: { notify() {}, subTaskCard() {}, workerInstruction() {}, workerError() {} },
    missionOrchestrator: Object.assign(new EventEmitter(), { ensureTodoManagerInitialized: async () => {} }),
    workspaceRoot: ROOT,
    getActiveUserPrompt: () => '', getActiveImagePaths: () => undefined, getCurrentSessionId: () => 'session-update-todo',
    getMissionIdsBySession: async () => [], ensureMissionForDispatch: async () => 'mission-update-todo', getCurrentTurnId: () => 'turn-update-todo',
    getProjectKnowledgeBase: () => undefined, processWorkerWisdom() {}, recordOrchestratorTokens() {}, recordWorkerTokenUsage() {},
    getSnapshotManager: () => null, getContextManager: () => null, getTodoManager: () => todoManager, getSupplementaryQueue: () => null,
  });
  manager.setupOrchestrationToolHandlers();
  assert(handlers && typeof handlers.updateTodo === 'function', '未成功注入 updateTodo handler');
  return { manager, updateTodo: handlers.updateTodo, interrupts };
}

async function testPendingDelegatesReset(DispatchManager) {
  let getCalled = false;
  const resetCalls = [];
  const ctx = createHarness(DispatchManager, { update: async () => {}, get: async () => { getCalled = true; return null; }, resetToPending: async (id, options) => resetCalls.push({ id, options }), skip: async () => {} });
  const result = await ctx.updateTodo({ updates: [{ todoId: 'todo-completed', status: 'pending' }] });
  assert(result.success === true, `pending reset 应成功，实际: ${JSON.stringify(result)}`);
  assert(getCalled === false, '非 forceReset 不应预读 Todo 状态做外层裁决');
  assert(resetCalls.length === 1 && resetCalls[0].id === 'todo-completed' && resetCalls[0].options.force === false, `resetToPending 调用异常: ${JSON.stringify(resetCalls)}`);
  ctx.manager.dispose();
}

async function testSkippedDelegatesSkip(DispatchManager) {
  const skipCalls = [];
  const ctx = createHarness(DispatchManager, { update: async () => {}, get: async () => null, resetToPending: async () => {}, skip: async (id) => skipCalls.push(id) });
  const result = await ctx.updateTodo({ updates: [{ todoId: 'todo-pending', status: 'skipped' }] });
  assert(result.success === true, `skip 应成功，实际: ${JSON.stringify(result)}`);
  assert(skipCalls.length === 1 && skipCalls[0] === 'todo-pending', `skip 调用异常: ${JSON.stringify(skipCalls)}`);
  ctx.manager.dispose();
}

async function testForceResetInterruptsRunningWorker(DispatchManager) {
  const resetCalls = [];
  const ctx = createHarness(DispatchManager, { update: async () => {}, get: async () => ({ id: 'todo-running', status: 'running', assignmentId: 'assignment-1', workerId: 'codex' }), resetToPending: async (id, options) => resetCalls.push({ id, options }), skip: async () => {} });
  ctx.manager.activeAssignments.set('assignment-1', { workerId: 'codex' });
  const result = await ctx.updateTodo({ updates: [{ todoId: 'todo-running', status: 'pending', forceReset: true }] });
  assert(result.success === true, `force reset 应成功，实际: ${JSON.stringify(result)}`);
  assert(ctx.interrupts.length === 1 && ctx.interrupts[0] === 'codex', `running forceReset 应中断 worker，实际: ${JSON.stringify(ctx.interrupts)}`);
  assert(resetCalls.length === 1 && resetCalls[0].options.force === true, `force reset 调用异常: ${JSON.stringify(resetCalls)}`);
  ctx.manager.dispose();
}

async function testInvalidTransitionDelegatedToTodoManager(DispatchManager) {
  const ctx = createHarness(DispatchManager, { update: async () => {}, get: async () => null, resetToPending: async () => { throw new Error('illegal-reset'); }, skip: async () => {} });
  const result = await ctx.updateTodo({ updates: [{ todoId: 'todo-invalid', status: 'pending' }] });
  assert(result.success === false, '非法转换应由 TodoManager 返回失败');
  assert(result.error === 'illegal-reset', `非法转换错误未透传 TodoManager: ${JSON.stringify(result)}`);
  ctx.manager.dispose();
}

async function main() {
  testSourceGuardrails();
  const { DispatchManager } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-manager.js'));
  await testPendingDelegatesReset(DispatchManager);
  await testSkippedDelegatesSkip(DispatchManager);
  await testForceResetInterruptsRunningWorker(DispatchManager);
  await testInvalidTransitionDelegatedToTodoManager(DispatchManager);
  console.log('\n=== dispatch todo_update regression ===');
  console.log(JSON.stringify({ pass: true, checks: ['pending_delegates_reset', 'skipped_delegates_skip', 'running_force_reset_interrupts', 'invalid_transition_delegated_to_todo_manager'] }, null, 2));
}

main().catch((error) => { console.error('dispatch todo_update 回归失败:', error?.stack || error); process.exit(1); });