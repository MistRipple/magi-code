/**
 * Wave 1-4 全量端到端验证脚本
 *
 * 验证全部 19 个修复项（#1-#19）的完整性。
 * 运行: npx ts-node --skip-project scripts/wave1-4-e2e-verify.ts
 */

import * as fs from 'fs';
import * as path from 'path';

const ROOT = path.resolve(process.cwd());
const SRC = path.join(ROOT, 'src');

let totalChecks = 0;
let passed = 0;
let failed = 0;

function check(name: string, condition: boolean, detail?: string): void {
  totalChecks++;
  if (condition) {
    passed++;
    console.log(`  ✅ ${name}`);
  } else {
    failed++;
    console.log(`  ❌ ${name}${detail ? ` — ${detail}` : ''}`);
  }
}

function readFile(relativePath: string): string {
  return fs.readFileSync(path.join(SRC, relativePath), 'utf-8');
}

function fileExists(relativePath: string): boolean {
  return fs.existsSync(path.join(SRC, relativePath));
}

function lineCount(content: string): number {
  return content.split('\n').length;
}

function countOccurrences(content: string, pattern: RegExp): number {
  return (content.match(pattern) || []).length;
}

// 预加载所有需要的文件
const mde = readFile('orchestrator/core/mission-driven-engine.ts');
const mo = readFile('orchestrator/core/mission-orchestrator.ts');
const dm = readFile('orchestrator/core/dispatch-manager.ts');
const db = readFile('orchestrator/core/dispatch-batch.ts');
const wvp = readFile('ui/webview-provider.ts');
const ebs = readFile('ui/event-binding-service.ts');
const wss = readFile('ui/worker-status-service.ts');
const pe = readFile('orchestrator/core/executors/planning-executor.ts');

// ============================================================================
// Wave 1 — P0 死代码清理 + 核心解耦 (#1-#4)
// ============================================================================
console.log('\n' + '='.repeat(70));
console.log('  Wave 1 — P0 死代码清理 + 核心解耦 (#1-#4)');
console.log('='.repeat(70));

// --- #1+#2: L3 重构 Phase 4 收尾 ---
console.log('\n--- #1+#2: L3 重构 Phase 4 收尾（死代码清理）---\n');
check('#1 _context.mission 在 MDE 零引用', !mde.includes('_context.mission'));
check('#1 _context.plan 在 MDE 零引用', !mde.includes('_context.plan'));
check('#1 MDE 无 get context() getter', !mde.includes('get context()'));
check('#1 MDE 无 get plan() getter', !mde.includes('get plan()'));
check('#1 emitSubTaskStatusCard 全局零引用', !mde.includes('emitSubTaskStatusCard') && !wvp.includes('emitSubTaskStatusCard'));
check('#1 buildSubTaskTitlePrefix 全局零引用', !mde.includes('buildSubTaskTitlePrefix') && !wvp.includes('buildSubTaskTitlePrefix'));
check('#2 MDE 无 reportTodoProgress 方法', !mde.includes('reportTodoProgress'));
check('#2 reportTodoProgress 已迁移到 DM', dm.includes('reportTodoProgress'));

// --- #3: activeBatch 所有权归位 ---
console.log('\n--- #3: activeBatch 所有权归位 ---\n');
check('#3 DispatchManagerDeps 无 getActiveBatch 闭包', !dm.includes('getActiveBatch:'));
check('#3 DispatchManagerDeps 无 setActiveBatch 闭包', !dm.includes('setActiveBatch'));
check('#3 DM 内部持有 activeBatch', dm.includes('private activeBatch'));
check('#3 DM 有 getActiveBatch() 公开方法', dm.includes('getActiveBatch()'));
check('#3 MDE 不持有 private activeBatch', !mde.includes('private activeBatch'));

// --- #4: MDE 职责提取 ---
console.log('\n--- #4: MDE 职责提取 ---\n');
check('#4 ResilientCompressorAdapter 文件存在', fileExists('orchestrator/core/resilient-compressor-adapter.ts'));
check('#4 TaskViewService 文件存在', fileExists('services/task-view-service.ts'));
check('#4 MDE 导入 configureResilientCompressor', mde.includes("configureResilientCompressor"));
check('#4 MDE 导入 TaskViewService', mde.includes('TaskViewService'));
check('#4 IntentGate 包含 parseClassificationResponse', readFile('orchestrator/intent-gate.ts').includes('parseClassificationResponse'));
const mdeLines = lineCount(mde);
check(`#4 MDE 行数 ~1000 (实际: ${mdeLines})`, mdeLines < 1100, `实际: ${mdeLines}`);

// ============================================================================
// Wave 2 — P1 事件架构治理 (#6-#11)
// ============================================================================
console.log('\n' + '='.repeat(70));
console.log('  Wave 2 — P1 事件架构治理 (#6-#11)');
console.log('='.repeat(70));

// --- #6+#7: 消除 setupEventForwarding + 消除双重消费 ---
console.log('\n--- #6+#7: 消除事件桥接 + 消除双重消费 ---\n');
check('#6 MDE 无 setupEventForwarding 方法', !mde.includes('setupEventForwarding'));
check('#6 MDE 无 reportTodoProgress 方法', !mde.includes('reportTodoProgress'));
check('#7 MDE 不监听 todoStarted (无双重消费)', !mde.includes("on('todoStarted'"));
check('#7 MDE 不监听 todoCompleted (无双重消费)', !mde.includes("on('todoCompleted'"));
check('#7 MDE 不监听 todoFailed (无双重消费)', !mde.includes("on('todoFailed'"));
// DM 监听 todo 事件用于 SubTaskCard 进度（合法的唯一消费路径之一）
check('#7 DM 监听 todoStarted (用于 SubTaskCard)', dm.includes("on('todoStarted'"));
check('#7 EBS 监听 todoStarted (用于 UI sendData)', ebs.includes("mo.on('todoStarted'"));

// --- #8: 三套事件系统职责明确化 ---
console.log('\n--- #8: 三套事件系统职责明确化 ---\n');
check('#8 MDE 不再桥接 MO→MessageHub (无 mo.on 代码)', !mde.includes("missionOrchestrator.on('"));
check('#8 DM 不直接 emit MO 事件', !dm.includes('.emit('));
check('#8 EBS 导入 globalEventBus', ebs.includes("import { globalEventBus }"));
check('#8 EBS 导入 WEBVIEW_MESSAGE_TYPES', ebs.includes('WEBVIEW_MESSAGE_TYPES'));

// --- #9: Phase 显式状态机 ---
console.log('\n--- #9: Phase 显式状态机 ---\n');
check('#9 DispatchBatch 定义 BatchPhase 类型', db.includes('BatchPhase'));
check('#9 BatchPhase 包含 active', db.includes("'active'"));
check('#9 BatchPhase 包含 summarizing', db.includes("'summarizing'"));
check('#9 BatchPhase 包含 archived', db.includes("'archived'"));
check('#9 DispatchBatch 有 transitionTo 方法', db.includes('transitionTo('));
check('#9 transitionTo 有合法路径校验', db.includes('ALLOWED_PHASE_TRANSITIONS'));

// --- #10+#11: MO 事件类型安全 + 收拢 emit 权限 ---
console.log('\n--- #10+#11: MO 事件类型安全 + 收拢 emit 权限 ---\n');
check('#10 MO 定义 MissionOrchestratorEventMap', mo.includes('MissionOrchestratorEventMap'));
// 检查 EventMap 事件参数有类型约束
check('#10 EventMap todoStarted 有类型参数', mo.includes("todoStarted: (data: {"));
check('#10 EventMap missionCreated 有类型参数', mo.includes("missionCreated: (data: {"));
check('#10 EventMap assignmentPlanned 有类型参数', mo.includes("assignmentPlanned: (data: {"));
// 外部不直接 emit
check('#11 外部无 missionOrchestrator.emit() 调用', !dm.includes('missionOrchestrator.emit('));
check('#11 MO 有 notifyAssignmentPlanned 方法', mo.includes('notifyAssignmentPlanned'));
check('#11 DM 使用 notifyAssignmentPlanned', dm.includes('notifyAssignmentPlanned'));

// ============================================================================
// Wave 3 — P2 质量提升 (#12-#16)
// ============================================================================
console.log('\n' + '='.repeat(70));
console.log('  Wave 3 — P2 质量提升 (#12-#16)');
console.log('='.repeat(70));

// --- #12: PlanningExecutor plan 模式死代码 ---
console.log('\n--- #12: PlanningExecutor plan 模式死代码删除 ---\n');
check('#12 PE 无 planWithLLM 方法', !pe.includes('planWithLLM'));
check('#12 PE 无 PlanningOptions 接口', !pe.includes('PlanningOptions'));
check('#12 PE 无 PlanningResult 接口', !pe.includes('PlanningResult'));
const peLines = lineCount(pe);
check(`#12 PE 行数合理 (实际: ${peLines})`, peLines < 100, `实际: ${peLines}`);

// --- #13: PlanningExecutor 改为单例注入 ---
console.log('\n--- #13: PlanningExecutor 单例注入 ---\n');
// DispatchManagerDeps 接口在 DM 文件顶部定义，检查其中不含 planningExecutor 字段
const depsInterface = dm.slice(dm.indexOf('interface DispatchManagerDeps'), dm.indexOf('}', dm.indexOf('interface DispatchManagerDeps')) + 1);
check('#13 DispatchManagerDeps 无 planningExecutor 工厂', !depsInterface.includes('planningExecutor'));
check('#13 DM 内部延迟创建 PlanningExecutor', dm.includes('_planningExecutor'));
check('#13 MDE 不再 import PlanningExecutor', !mde.includes("import { PlanningExecutor }") && !mde.includes("import type { PlanningExecutor }"));

// --- #14: Phase C 降级透明化 ---
console.log('\n--- #14: Phase C 降级透明化 ---\n');
check('#14 DM 有 phaseCFallback 方法', dm.includes('phaseCFallback'));
check('#14 降级时发送 warning 通知', dm.includes("messageHub.notify('汇总模型调用失败"));
check('#14 降级非静默（包含 warning 级别）', dm.includes("'warning'"));

// --- #15: 串行队列用户反馈 ---
console.log('\n--- #15: 串行队列用户反馈 ---\n');
check('#15 MDE 有 enqueueExecution 方法', mde.includes('enqueueExecution'));
check('#15 有 pendingCount 计数器', mde.includes('pendingCount'));
check('#15 队列非空时通知用户', mde.includes('任务排队中'));

// --- #16: WVP as any 清除 ---
console.log('\n--- #16: WVP as any 清除 ---\n');
const wvpAsAny = countOccurrences(wvp, /as any/g);
check(`#16 WVP 中 as any 数量 = 0 (实际: ${wvpAsAny})`, wvpAsAny === 0, `发现 ${wvpAsAny} 处`);

// ============================================================================
// Wave 4 — P3 组件拆分 (#17-#19)
// ============================================================================
console.log('\n' + '='.repeat(70));
console.log('  Wave 4 — P3 组件拆分 (#17-#19)');
console.log('='.repeat(70));

// --- #17: MO 拆分 ---
console.log('\n--- #17: MissionOrchestrator 拆分 ---\n');
const moLines = lineCount(mo);
check(`#17 MO 行数 < 300 (实际: ${moLines})`, moLines < 300, `实际: ${moLines}`);

// 死方法验证
const moDeadMethods = [
  'processRequest', 'createMission', 'understandGoal', 'selectParticipants',
  'defineContracts', 'assignResponsibilities',
  'approveMission', 'pauseMission', 'resumeMission', 'cancelMission',
  'completeMission', 'failMission',
  'verifyMission', 'verifyCriterion', 'summarizeMission',
  'getCachedPlanning', 'cachePlanning', 'clearCache',
  'getSnapshotManager', 'getContextManager', 'getProfileLoader',
  'getGuidanceInjector', 'getReviewer', 'getIntentGate',
];
for (const method of moDeadMethods) {
  check(`#17 MO 无死方法 '${method}'`, !mo.includes(`${method}(`));
}

// 死类型验证
const moDeadTypes = [
  'MissionCreationResult', 'ExecutionOptions', 'ExecutionProgress',
  'ExecutionResult', 'MissionVerificationResult', 'MissionSummary',
];
for (const type of moDeadTypes) {
  check(`#17 MO 无死类型 '${type}'`, !mo.includes(`interface ${type}`) && !mo.includes(`type ${type}`));
}

// 死字段验证
const moDeadFields = [
  'contractManager', 'assignmentManager', 'reviewer',
  'verificationRunner', 'planningCache', 'assignmentResolver',
];
for (const field of moDeadFields) {
  check(`#17 MO 无死字段 '${field}'`, !mo.includes(`private ${field}`) && !mo.includes(`readonly ${field}`));
}

// EventMap 事件数
const moEvents = [
  'missionCreated', 'missionStatusChanged', 'missionPhaseChanged',
  'workerSessionCreated', 'workerSessionResumed',
  'todoStarted', 'todoCompleted', 'todoFailed', 'dynamicTodoAdded',
  'insightGenerated',
  'assignmentPlanned', 'assignmentStarted', 'assignmentCompleted',
  'approvalRequested',
];
for (const event of moEvents) {
  check(`#17 MO EventMap 定义 '${event}'`, mo.includes(`${event}:`));
}

// --- #18: WVP 瘦身 ---
console.log('\n--- #18: WebviewProvider 瘦身 ---\n');
const wvpLines = lineCount(wvp);
check(`#18 WVP 行数 < 2700 (实际: ${wvpLines})`, wvpLines < 2700, `实际: ${wvpLines}`);
const ebsLines = lineCount(ebs);
check(`#18 EBS 存在且 > 400 行 (实际: ${ebsLines})`, ebsLines > 400, `实际: ${ebsLines}`);
const wssLines = lineCount(wss);
check(`#18 WSS 存在且 > 200 行 (实际: ${wssLines})`, wssLines > 200, `实际: ${wssLines}`);

// WVP 不含已提取方法
const extractedMethods = [
  'setupAdapterEvents', 'setupMessageHubListeners', 'bindEvents',
  'bindMissionEvents', 'clearActiveToolAuthorizationTimer',
  'pumpToolAuthorizationQueue', 'performWorkerStatusCheck',
];
for (const method of extractedMethods) {
  check(`#18 WVP 无已提取方法 '${method}'`, !wvp.includes(`private ${method}(`));
}

// WVP 不含已提取字段
const extractedFields = [
  'toolAuthorizationCallbacks', 'toolAuthorizationQueue',
  'activeToolAuthorizationRequestId', 'activeToolAuthorizationTimer',
  'workerStatusCache', 'workerStatusCacheAt', 'workerStatusInFlight',
  'workerStatusCacheTtlMs', 'workerStatusSoftTtlMs',
];
for (const field of extractedFields) {
  check(`#18 WVP 无已提取字段 '${field}'`, !wvp.includes(`private ${field}`));
}

// WVP 导入验证
check('#18 WVP 导入 EventBindingService', wvp.includes("import { EventBindingService }"));
check('#18 WVP 导入 WorkerStatusService', wvp.includes("import { WorkerStatusService }"));
check('#18 WVP 不导入 WEBVIEW_MESSAGE_TYPES (已提取)', !wvp.includes('WEBVIEW_MESSAGE_TYPES'));
check('#18 WVP 不导入 AgentType (死导入)', !wvp.includes("from '../types/agent-types'"));

// EBS 接口契约
const ebcMethods = [
  'getActiveSessionId', 'getMessageHub', 'getOrchestratorEngine',
  'getAdapterFactory', 'getMissionOrchestrator', 'getMessageIdToRequestId',
  'sendStateUpdate', 'sendData', 'sendToast', 'sendExecutionStats',
  'sendOrchestratorMessage', 'appendLog', 'postMessage', 'logMessageFlow',
  'resolveRequestTimeoutFromMessage', 'clearRequestTimeout',
  'interruptCurrentTask', 'tryResumePendingRecovery',
];
for (const method of ebcMethods) {
  check(`#18 EventBindingContext 声明 ${method}`, ebs.includes(`${method}(`));
}

// EBS 监听 MO 事件
const ebsMOEvents = [
  'missionCreated', 'missionStatusChanged',
  'assignmentStarted', 'assignmentPlanned', 'assignmentCompleted',
  'workerSessionCreated', 'workerSessionResumed',
  'todoStarted', 'todoCompleted', 'todoFailed',
  'dynamicTodoAdded', 'approvalRequested',
];
for (const event of ebsMOEvents) {
  check(`#18 EBS 监听 MO '${event}'`, ebs.includes(`mo.on('${event}'`));
}

// --- #19: 资源生命周期管理 ---
console.log('\n--- #19: 资源生命周期管理 ---\n');
check('#19 WVP.dispose() 调用 orchestratorEngine.dispose()', wvp.includes('this.orchestratorEngine.dispose()'));
check('#19 WVP.dispose() 调用 eventBindingService.disposeToolAuthorization()', wvp.includes('this.eventBindingService.disposeToolAuthorization()'));
check('#19 WVP.dispose() 调用 globalEventBus.clear()', wvp.includes('globalEventBus.clear()'));
check('#19 MDE.dispose() 调用 dispatchManager.dispose()', mde.includes('this.dispatchManager.dispose()'));
check('#19 MDE.dispose() 调用 messageHub.dispose()', mde.includes('this.messageHub.dispose()'));
check('#19 MDE.dispose() 调用 missionOrchestrator.dispose()', mde.includes('this.missionOrchestrator.dispose()'));
check('#19 MDE.dispose() 调用 removeAllListeners()', mde.includes('this.removeAllListeners()'));
check('#19 MO.dispose() 调用 worker.dispose()', mo.includes('worker.dispose()'));
check('#19 MO.dispose() 调用 workers.clear()', mo.includes('this.workers.clear()'));
check('#19 MO.dispose() 调用 removeAllListeners()', mo.includes('this.removeAllListeners()'));
check('#19 DM 有 dispose() 方法', dm.includes('dispose(): void'));

// ============================================================================
// 全局不变量
// ============================================================================
console.log('\n' + '='.repeat(70));
console.log('  全局不变量验证');
console.log('='.repeat(70));

// --- 文件规模 ---
console.log('\n--- 文件规模合理性 ---\n');
const dmLines = lineCount(dm);
check(`全局 MDE 行数 < 1100 (实际: ${mdeLines})`, mdeLines < 1100);
check(`全局 MO 行数 < 300 (实际: ${moLines})`, moLines < 300);
check(`全局 WVP 行数 < 2700 (实际: ${wvpLines})`, wvpLines < 2700);
check(`全局 DM 行数 < 700 (实际: ${dmLines})`, dmLines < 700);

// --- 编码规范 ---
console.log('\n--- 编码规范 ---\n');
check('EBS 无 as any', countOccurrences(ebs, /as any/g) === 0);
check('WSS as any 合理 (<=3)', countOccurrences(wss, /as any/g) <= 3);
check('EBS 使用 LogCategory', ebs.includes('LogCategory'));
check('WSS 使用 LogCategory', wss.includes('LogCategory'));

// --- 禁止多重实现 ---
console.log('\n--- 禁止多重实现 ---\n');
// Todo 事件不被 MDE 双重消费
check('MDE 不监听 todoStarted', !mde.includes("on('todoStarted'"));
check('MDE 不监听 todoCompleted', !mde.includes("on('todoCompleted'"));
check('MDE 不监听 todoFailed', !mde.includes("on('todoFailed'"));
// MDE 不桥接 MO 事件
check('MDE 无 setupEventForwarding', !mde.includes('setupEventForwarding'));

// --- 禁止打补丁 ---
console.log('\n--- 禁止打补丁 ---\n');
check('Phase C 降级有 warning 通知', dm.includes("notify('汇总模型调用失败"));
check('MDE 无事件桥接补丁', !mde.includes('setupEventForwarding'));

// ============================================================================
// 结果汇总
// ============================================================================
console.log('\n' + '='.repeat(70));

// 统计各 Wave 的结果
console.log(`\n验证完成: ${passed}/${totalChecks} 通过, ${failed} 失败`);
console.log(`\n  Wave 1 (#1-#4):  死代码清理 + 核心解耦`);
console.log(`  Wave 2 (#6-#11): 事件架构治理`);
console.log(`  Wave 3 (#12-#16): 质量提升`);
console.log(`  Wave 4 (#17-#19): 组件拆分`);
console.log(`  全局不变量\n`);

if (failed > 0) {
  console.log('⚠️  存在失败项，请检查上述 ❌ 标记');
  process.exit(1);
} else {
  console.log('✅ 全部验证通过！');
  process.exit(0);
}
