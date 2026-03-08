#!/usr/bin/env node
/**
 * Plan 治理门控回归脚本
 *
 * 覆盖目标：
 * 1) 低风险 + 高置信度 => auto
 * 2) 高风险 => ask
 * 3) 低置信度 => ask
 * 4) 评估异常兜底 => ask
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

const originalModuleLoad = Module._load;
Module._load = function patchedModuleLoad(request, parent, isMain) {
  if (request === 'vscode') {
    return {};
  }
  return originalModuleLoad.call(this, request, parent, isMain);
};

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function testSourceGuardrails() {
  const source = fs.readFileSync(path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts'), 'utf8');
  assert(source.includes('terminationMetricsRepository.append('), 'MissionDrivenEngine 未通过统一仓储接口写入终止指标');
  assert(source.includes('FileTerminationMetricsRepository'), 'MissionDrivenEngine 未接入文件仓储实现');
}

function createPlan(planId, mode, files) {
  return {
    planId,
    sessionId: 'session-governance',
    turnId: 'turn-governance',
    version: 1,
    mode,
    status: 'draft',
    source: 'orchestrator',
    promptDigest: 'digest',
    summary: 'summary',
    acceptanceCriteria: [],
    constraints: [],
    items: [{
      itemId: `${planId}-item-1`,
      title: 'item',
      owner: 'codex',
      status: 'pending',
      progress: 0,
      dependsOn: [],
      scopeHints: [],
      targetFiles: files,
      todoIds: [],
      todoStatuses: {},
      createdAt: Date.now(),
      updatedAt: Date.now(),
    }],
    attempts: [],
    links: {
      assignmentIds: [],
      todoIds: [],
    },
    createdAt: Date.now(),
    updatedAt: Date.now(),
  };
}

function createHistoryPlans(count, options = {}) {
  const plans = [];
  for (let i = 0; i < count; i++) {
    plans.push({
      planId: `history-${i}`,
      status: i < (options.failedCount || 0) ? 'failed' : 'completed',
      items: [{
        targetFiles: options.files || ['src/core/a.ts'],
      }],
    });
  }
  return plans;
}

function createEngineHarness(MissionDrivenEngine, historyPlans, indexFiles) {
  const engine = Object.create(MissionDrivenEngine.prototype);
  engine.workspaceRoot = ROOT;
  engine.terminationMetricsRepository = {
    append(record) {
      const metricsDir = path.join(ROOT, '.magi', 'metrics');
      const metricsPath = path.join(metricsDir, 'termination.jsonl');
      fs.mkdirSync(metricsDir, { recursive: true });
      fs.appendFileSync(metricsPath, `${JSON.stringify(record)}\n`, 'utf8');
    },
  };
  engine.workspaceFileIndexCache = {
    builtAt: Date.now(),
    files: indexFiles,
    modules: Array.from(new Set(indexFiles.map((file) => file.split('/')[0] || file))),
  };
  engine.planLedger = {
    listPlans() {
      return historyPlans;
    },
  };
  return engine;
}

async function main() {
  testSourceGuardrails();
  const { MissionDrivenEngine } = loadCompiledModule(path.join('orchestrator', 'core', 'mission-driven-engine.js'));

  // 1) 低风险 + 高置信度 => auto
  {
    const history = createHistoryPlans(12, {
      failedCount: 1,
      files: ['src/core/a.ts'],
    });
    const engine = createEngineHarness(MissionDrivenEngine, history, [
      'src/core/a.ts',
      'src/core/b.ts',
      'docs/readme.md',
    ]);
    const plan = createPlan('plan-auto', 'standard', ['src/core/a.ts']);
    const assessment = await engine.evaluatePlanGovernance(
      'session-governance',
      plan,
      '请只读分析 src/core/a.ts 并给出总结',
    );
    assert(assessment.decision === 'auto', `低风险高置信应 auto，实际: ${assessment.decision}`);
    assert(assessment.riskScore <= 0.35, `低风险阈值异常: ${assessment.riskScore}`);
    assert(assessment.confidence >= 0.75, `高置信度阈值异常: ${assessment.confidence}`);
  }

  // 2) 高风险 => ask
  {
    const highRiskFiles = [];
    for (let i = 0; i < 60; i++) {
      highRiskFiles.push(`module${i % 12}/file${i}.ts`);
    }
    const history = createHistoryPlans(12, {
      failedCount: 8,
      files: highRiskFiles,
    });
    const engine = createEngineHarness(MissionDrivenEngine, history, highRiskFiles);
    const plan = createPlan('plan-high-risk', 'deep', highRiskFiles);
    const assessment = await engine.evaluatePlanGovernance(
      'session-governance',
      plan,
      '执行跨模块重构并写入大量文件',
    );
    assert(assessment.decision === 'ask', `高风险应 ask，实际: ${assessment.decision}`);
    assert(assessment.riskScore >= 0.70, `高风险阈值异常: ${assessment.riskScore}`);
  }

  // 3) 低置信度 => ask
  {
    const engine = createEngineHarness(MissionDrivenEngine, [], ['src/core/a.ts']);
    const plan = createPlan('plan-low-confidence', 'standard', []);
    const assessment = await engine.evaluatePlanGovernance(
      'session-governance',
      plan,
      'go',
    );
    assert(assessment.decision === 'ask', `低置信度应 ask，实际: ${assessment.decision}`);
    assert(assessment.confidence < 0.55, `低置信度阈值异常: ${assessment.confidence}`);
  }

  // 4) 评估异常兜底 => ask
  {
    const engine = createEngineHarness(MissionDrivenEngine, [], ['src/core/a.ts']);
    const fallback = engine.buildFallbackGovernanceAssessment(new Error('synthetic governance error'));
    assert(fallback.decision === 'ask', `异常兜底应 ask，实际: ${fallback.decision}`);
    assert(fallback.confidence === 0, `异常兜底 confidence 应为 0，实际: ${fallback.confidence}`);
  }

  // 5) 终止指标落盘 => termination.jsonl 追加记录
  {
    const engine = createEngineHarness(MissionDrivenEngine, [], ['src/core/a.ts']);
    const metricsPath = path.join(ROOT, '.magi', 'metrics', 'termination.jsonl');
    const beforeLines = fs.existsSync(metricsPath)
      ? fs.readFileSync(metricsPath, 'utf8').split('\n').filter(Boolean).length
      : 0;

    engine.persistTerminationMetrics({
      sessionId: 'session-governance',
      planId: 'plan-metrics',
      turnId: 'turn-metrics',
      mode: 'standard',
      finalPlanStatus: 'completed',
      runtimeReason: 'completed',
      runtimeRounds: 1,
      runtimeSnapshot: {
        sourceEventIds: ['metrics-evidence'],
        progressVector: {
          terminalRequiredTodos: 1,
          acceptedCriteria: 1,
          criticalPathResolved: 1,
          unresolvedBlockers: 0,
        },
        budgetState: {
          elapsedMs: 1000,
          tokenUsed: 123,
          errorRate: 0,
        },
      },
      runtimeShadow: {
        enabled: true,
        reason: 'completed',
        consistent: true,
      },
      tokenUsage: {
        inputTokens: 10,
        outputTokens: 20,
      },
      startedAt: Date.now() - 1000,
    });

    assert(fs.existsSync(metricsPath), 'termination.jsonl 未生成');
    const afterLinesRaw = fs.readFileSync(metricsPath, 'utf8').split('\n').filter(Boolean);
    assert(afterLinesRaw.length === beforeLines + 1, `termination.jsonl 未追加新记录: before=${beforeLines}, after=${afterLinesRaw.length}`);
    const latest = JSON.parse(afterLinesRaw[afterLinesRaw.length - 1]);
    assert(latest.plan_id === 'plan-metrics', `落盘 plan_id 异常: ${latest.plan_id}`);
    assert(latest.reason === 'completed', `落盘 reason 异常: ${latest.reason}`);
    assert(Array.isArray(latest.evidence_ids) && latest.evidence_ids.includes('metrics-evidence'), '落盘 evidence_ids 异常');
  }

  console.log('\n=== plan governance gate regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'low-risk-auto',
      'high-risk-ask',
      'low-confidence-ask',
      'fallback-ask',
      'termination-metrics-persisted',
    ],
  }, null, 2));
  process.exit(0);
}

main().catch((error) => {
  console.error('plan governance gate 回归失败:', error?.stack || error);
  process.exit(1);
});
