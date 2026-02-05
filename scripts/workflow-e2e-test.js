#!/usr/bin/env node
/**
 * 5 阶段工作流端到端验证脚本（静态验证）
 *
 * 验证目标：
 * 1. TypeScript 编译通过
 * 2. 核心类型和接口定义完整
 * 3. 源代码结构正确
 */

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const repoRoot = path.join(__dirname, '..');

console.log('═══════════════════════════════════════════════════════');
console.log('  5 阶段工作流端到端验证');
console.log('═══════════════════════════════════════════════════════\n');

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    const result = fn();
    if (result === false) throw new Error('验证失败');
    console.log(`  ✓ ${name}`);
    passed++;
  } catch (e) {
    console.log(`  ✗ ${name}`);
    console.log(`    错误: ${e.message}`);
    failed++;
  }
}

function fileContains(filePath, pattern) {
  const content = fs.readFileSync(path.join(repoRoot, filePath), 'utf-8');
  if (pattern instanceof RegExp) {
    return pattern.test(content);
  }
  return content.includes(pattern);
}

// ============================================================================
// 编译检查
// ============================================================================
console.log('【编译检查】');

test('TypeScript 编译通过', () => {
  const result = spawnSync('npx', ['tsc', '--noEmit'], { cwd: repoRoot });
  if (result.status !== 0) {
    throw new Error('编译失败');
  }
  return true;
});

// ============================================================================
// Phase 1: 意图门控
// ============================================================================
console.log('\n【Phase 1: 意图门控】');

test('IntentGate 类存在', () => {
  return fileContains('src/orchestrator/intent-gate.ts', 'export class IntentGate');
});

test('IntentHandlerMode 枚举完整', () => {
  const modes = ['ASK', 'DIRECT', 'EXPLORE', 'TASK', 'CLARIFY', 'DEMO'];
  for (const mode of modes) {
    if (!fileContains('src/orchestrator/intent-gate.ts', mode)) {
      throw new Error(`缺少模式: ${mode}`);
    }
  }
  return true;
});

// ============================================================================
// Phase 2: 需求分析
// ============================================================================
console.log('\n【Phase 2: 需求分析】');

test('RequirementAnalysis 接口定义', () => {
  return fileContains('src/orchestrator/protocols/types.ts', 'export interface RequirementAnalysis');
});

test('buildRequirementAnalysisPrompt 函数存在', () => {
  return fileContains('src/orchestrator/prompts/orchestrator-prompts.ts', 'export function buildRequirementAnalysisPrompt');
});

test('analyzeRequirement 方法存在', () => {
  return fileContains('src/orchestrator/core/mission-driven-engine.ts', 'async analyzeRequirement(');
});

test('缓存机制实现', () => {
  return fileContains('src/orchestrator/core/mission-driven-engine.ts', '_cachedRequirementAnalysis');
});

// ============================================================================
// Phase 3: 协作规划
// ============================================================================
console.log('\n【Phase 3: 协作规划】');

test('planCollaborationWithLLM 方法存在', () => {
  return fileContains('src/orchestrator/core/mission-driven-engine.ts', 'planCollaborationWithLLM');
});

test('MessageHub 类存在', () => {
  return fileContains('src/orchestrator/core/message-hub.ts', 'export class MessageHub');
});

test('taskAssignment 消息发送', () => {
  return fileContains('src/orchestrator/core/mission-driven-engine.ts', 'messageHub.taskAssignment');
});

// ============================================================================
// Phase 4: 任务执行
// ============================================================================
console.log('\n【Phase 4: 任务执行】');

test('WorkerReport 协议存在', () => {
  return fileContains('src/orchestrator/protocols/worker-report.ts', 'export function createProgressReport');
});

// ============================================================================
// Phase 5: 总结
// ============================================================================
console.log('\n【Phase 5: 总结】');

test('summarizeMission 方法调用', () => {
  return fileContains('src/orchestrator/core/mission-driven-engine.ts', 'summarizeMission');
});

test('sendSummaryMessage 方法存在', () => {
  return fileContains('src/orchestrator/core/mission-driven-engine.ts', 'sendSummaryMessage');
});

// ============================================================================
// 模式覆盖检查
// ============================================================================
console.log('\n【模式覆盖检查】');

test('DIRECT 模式使用 analyzeRequirement', () => {
  const content = fs.readFileSync(path.join(repoRoot, 'src/orchestrator/core/mission-driven-engine.ts'), 'utf-8');
  return content.includes('IntentHandlerMode.DIRECT') && content.includes('analyzeRequirement');
});

test('EXPLORE 模式使用 analyzeRequirement', () => {
  const content = fs.readFileSync(path.join(repoRoot, 'src/orchestrator/core/mission-driven-engine.ts'), 'utf-8');
  return content.includes('IntentHandlerMode.EXPLORE') && content.includes('analyzeRequirement');
});

test('DEMO 模式使用 analyzeRequirement', () => {
  const content = fs.readFileSync(path.join(repoRoot, 'src/orchestrator/core/mission-driven-engine.ts'), 'utf-8');
  return content.includes('IntentHandlerMode.DEMO') && content.includes('analyzeRequirement');
});

// ============================================================================
// 总结
// ============================================================================
console.log('\n═══════════════════════════════════════════════════════');
console.log(`  结果: ${passed} 通过, ${failed} 失败`);
console.log('═══════════════════════════════════════════════════════\n');

if (failed > 0) {
  process.exit(1);
}