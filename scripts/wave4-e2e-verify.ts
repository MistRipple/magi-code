/**
 * Wave 4 端到端验证脚本
 *
 * 验证 #17 MO 拆分 + #18 WVP 瘦身 + #19 Dispose 链 的完整性。
 * 运行: npx ts-node --skip-project scripts/wave4-e2e-verify.ts
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

function countOccurrences(content: string, pattern: RegExp): number {
  return (content.match(pattern) || []).length;
}

// ============================================================================
// 1. 文件规模验证
// ============================================================================
console.log('\n=== 1. 文件规模验证 ===\n');

const wvp = readFile('ui/webview-provider.ts');
const wvpLines = wvp.split('\n').length;
check('WVP 行数 < 2700', wvpLines < 2700, `实际: ${wvpLines}`);

const ebs = readFile('ui/event-binding-service.ts');
const ebsLines = ebs.split('\n').length;
check('EventBindingService 存在且 > 400 行', ebsLines > 400, `实际: ${ebsLines}`);

const wss = readFile('ui/worker-status-service.ts');
const wssLines = wss.split('\n').length;
check('WorkerStatusService 存在且 > 200 行', wssLines > 200, `实际: ${wssLines}`);

const mo = readFile('orchestrator/core/mission-orchestrator.ts');
const moLines = mo.split('\n').length;
check('MissionOrchestrator < 300 行', moLines < 300, `实际: ${moLines}`);

// ============================================================================
// 2. 导入完整性验证
// ============================================================================
console.log('\n=== 2. 导入完整性验证 ===\n');

check('WVP 导入 EventBindingService', wvp.includes("import { EventBindingService }"));
check('WVP 导入 WorkerStatusService', wvp.includes("import { WorkerStatusService }"));
check('WVP 不导入 WEBVIEW_MESSAGE_TYPES', !wvp.includes('WEBVIEW_MESSAGE_TYPES'));
check('WVP 不导入 AgentType', !wvp.includes("from '../types/agent-types'"));
check('WVP 不导入 Mission/Assignment 类型', !wvp.includes("from '../orchestrator/mission'"));

check('EventBindingService 导入 globalEventBus', ebs.includes("import { globalEventBus }"));
check('EventBindingService 导入 ADAPTER_EVENTS', ebs.includes('ADAPTER_EVENTS'));
check('EventBindingService 导入 PROCESSING_EVENTS', ebs.includes('PROCESSING_EVENTS'));
check('EventBindingService 导入 WEBVIEW_MESSAGE_TYPES', ebs.includes('WEBVIEW_MESSAGE_TYPES'));

// ============================================================================
// 3. 接口契约验证
// ============================================================================
console.log('\n=== 3. 接口契约验证 ===\n');

// EventBindingContext 方法列表
const ebcMethods = [
  'getActiveSessionId', 'getMessageHub', 'getOrchestratorEngine',
  'getAdapterFactory', 'getMissionOrchestrator', 'getMessageIdToRequestId',
  'sendStateUpdate', 'sendData', 'sendToast', 'sendExecutionStats',
  'sendOrchestratorMessage', 'appendLog', 'postMessage', 'logMessageFlow',
  'resolveRequestTimeoutFromMessage', 'clearRequestTimeout',
  'interruptCurrentTask', 'tryResumePendingRecovery',
];
for (const method of ebcMethods) {
  check(`EventBindingContext 声明 ${method}`, ebs.includes(`${method}(`));
}

// WorkerStatusContext 方法列表
const wscMethods = ['sendData', 'getAdapterFactory'];
for (const method of wscMethods) {
  check(`WorkerStatusContext 声明 ${method}`, wss.includes(`${method}(`));
}

// ============================================================================
// 4. Dispose 链验证
// ============================================================================
console.log('\n=== 4. Dispose 链验证 ===\n');

check('WVP.dispose() 调用 orchestratorEngine.dispose()', wvp.includes('this.orchestratorEngine.dispose()'));
check('WVP.dispose() 调用 eventBindingService.disposeToolAuthorization()', wvp.includes('this.eventBindingService.disposeToolAuthorization()'));
check('WVP.dispose() 调用 globalEventBus.clear()', wvp.includes('globalEventBus.clear()'));

const mde = readFile('orchestrator/core/mission-driven-engine.ts');
check('MDE.dispose() 调用 dispatchManager.dispose()', mde.includes('this.dispatchManager.dispose()'));
check('MDE.dispose() 调用 messageHub.dispose()', mde.includes('this.messageHub.dispose()'));
check('MDE.dispose() 调用 missionOrchestrator.dispose()', mde.includes('this.missionOrchestrator.dispose()'));
check('MDE.dispose() 调用 removeAllListeners()', mde.includes('this.removeAllListeners()'));

check('MO.dispose() 调用 worker.dispose()', mo.includes('worker.dispose()'));
check('MO.dispose() 调用 workers.clear()', mo.includes('this.workers.clear()'));
check('MO.dispose() 调用 removeAllListeners()', mo.includes('this.removeAllListeners()'));

const dm = readFile('orchestrator/core/dispatch-manager.ts');
check('DM 有 dispose() 方法', dm.includes('dispose(): void'));

// ============================================================================
// 5. 事件流验证
// ============================================================================
console.log('\n=== 5. 事件流验证 ===\n');

// MO EventMap 事件
const moEvents = [
  'missionCreated', 'missionStatusChanged', 'missionPhaseChanged',
  'workerSessionCreated', 'workerSessionResumed',
  'todoStarted', 'todoCompleted', 'todoFailed', 'dynamicTodoAdded',
  'insightGenerated',
  'assignmentPlanned', 'assignmentStarted', 'assignmentCompleted',
  'approvalRequested',
];

for (const event of moEvents) {
  check(`MO EventMap 定义 '${event}'`, mo.includes(`${event}:`));
}

// EventBindingService 监听的 MO 事件
const ebsMOEvents = [
  'missionCreated', 'missionStatusChanged',
  'assignmentStarted', 'assignmentPlanned', 'assignmentCompleted',
  'workerSessionCreated', 'workerSessionResumed',
  'todoStarted', 'todoCompleted', 'todoFailed',
  'dynamicTodoAdded', 'approvalRequested',
];
for (const event of ebsMOEvents) {
  check(`EventBindingService 监听 MO '${event}'`, ebs.includes(`mo.on('${event}'`));
}

// ============================================================================
// 6. 死代码验证
// ============================================================================
console.log('\n=== 6. 死代码验证 ===\n');

// WVP 不应包含已提取的方法
const extractedMethods = [
  'setupAdapterEvents', 'setupMessageHubListeners', 'bindEvents',
  'bindMissionEvents', 'clearActiveToolAuthorizationTimer',
  'pumpToolAuthorizationQueue', 'performWorkerStatusCheck',
];
for (const method of extractedMethods) {
  check(`WVP 不包含已提取方法 '${method}'`, !wvp.includes(`private ${method}(`));
}

// WVP 不应包含已提取的字段
const extractedFields = [
  'toolAuthorizationCallbacks', 'toolAuthorizationQueue',
  'activeToolAuthorizationRequestId', 'activeToolAuthorizationTimer',
  'workerStatusCache', 'workerStatusCacheAt', 'workerStatusInFlight',
  'workerStatusCacheTtlMs', 'workerStatusSoftTtlMs',
];
for (const field of extractedFields) {
  check(`WVP 不包含已提取字段 '${field}'`, !wvp.includes(`private ${field}`));
}

// ============================================================================
// 7. 编码规范验证
// ============================================================================
console.log('\n=== 7. 编码规范验证 ===\n');

// 新增文件中无 as any
const ebsAnyCount = countOccurrences(ebs, /as any/g);
check('EventBindingService 无 as any', ebsAnyCount === 0, `发现 ${ebsAnyCount} 处`);

// WorkerStatusService 中的 as any 仅在 modelConfig 参数处（这是从 WVP 原封不动迁移的）
const wssAsAny = countOccurrences(wss, /as any/g);
// modelConfig 参数类型就是 any，这是合理的
check('WorkerStatusService as any 数量合理 (≤3)', wssAsAny <= 3, `发现 ${wssAsAny} 处`);

// 日志使用 LogCategory
check('EventBindingService 使用 LogCategory', ebs.includes('LogCategory'));
check('WorkerStatusService 使用 LogCategory', wss.includes('LogCategory'));

// ============================================================================
// 结果汇总
// ============================================================================
console.log('\n' + '='.repeat(60));
console.log(`\n验证完成: ${passed}/${totalChecks} 通过, ${failed} 失败\n`);

if (failed > 0) {
  console.log('⚠️  存在失败项，请检查上述 ❌ 标记');
  process.exit(1);
} else {
  console.log('✅ 全部验证通过！');
  process.exit(0);
}
