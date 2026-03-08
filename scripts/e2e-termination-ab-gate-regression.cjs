#!/usr/bin/env node
/**
 * termination ab gate 回归脚本
 *
 * 覆盖目标：
 * 1) 低偏差样本应通过
 * 2) 高偏差样本应被闸门拦截
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const { runTerminationAbGate } = require('./verify-termination-ab-gate.cjs');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function writeJsonl(filePath, records) {
  const lines = records.map(record => JSON.stringify(record));
  fs.writeFileSync(filePath, `${lines.join('\n')}\n`, 'utf8');
}

function buildRecord(seed, reason, shadowConsistent = true) {
  return {
    timestamp: new Date().toISOString(),
    session_id: `session-${seed}`,
    plan_id: `plan-${seed}`,
    turn_id: `turn-${seed}`,
    mode: 'standard',
    final_status: reason === 'completed' ? 'completed' : 'failed',
    reason,
    rounds: 2,
    duration_ms: 1200 + seed,
    token_used: 200 + seed,
    evidence_ids: [`e-${seed}`],
    progress_vector: {
      terminalRequiredTodos: 2,
      acceptedCriteria: reason === 'completed' ? 2 : 1,
      criticalPathResolved: reason === 'completed' ? 1 : 0.5,
      unresolvedBlockers: reason === 'completed' ? 0 : 1,
    },
    shadow: {
      enabled: true,
      reason,
      consistent: shadowConsistent,
    },
  };
}

function createFixturePairPass(baseDir) {
  const baselinePath = path.join(baseDir, 'baseline-pass.jsonl');
  const candidatePath = path.join(baseDir, 'candidate-pass.jsonl');

  const baseline = [];
  const candidate = [];
  for (let i = 0; i < 30; i++) {
    const reason = i < 24 ? 'completed' : 'failed';
    baseline.push(buildRecord(i, reason, true));
    candidate.push(buildRecord(i, reason, true));
  }

  writeJsonl(baselinePath, baseline);
  writeJsonl(candidatePath, candidate);
  return { baselinePath, candidatePath };
}

function createFixturePairFail(baseDir) {
  const baselinePath = path.join(baseDir, 'baseline-fail.jsonl');
  const candidatePath = path.join(baseDir, 'candidate-fail.jsonl');

  const baseline = [];
  const candidate = [];
  for (let i = 0; i < 30; i++) {
    baseline.push(buildRecord(i, 'completed', true));
    candidate.push(buildRecord(i, 'failed', false));
  }

  writeJsonl(baselinePath, baseline);
  writeJsonl(candidatePath, candidate);
  return { baselinePath, candidatePath };
}

function main() {
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-ab-gate-'));
  try {
    const passFixture = createFixturePairPass(workspaceRoot);
    const passReport = runTerminationAbGate({
      root: workspaceRoot,
      baselinePath: passFixture.baselinePath,
      candidatePath: passFixture.candidatePath,
      minOverlapSamples: 10,
      maxOverlapMismatchRate: 0.01,
      maxDistributionDrift: 0.05,
      maxShadowConsistencyDrop: 0.01,
    });
    assert(passReport.gate.pass === true, `低偏差样本应通过: ${JSON.stringify(passReport.gate)}`);

    const failFixture = createFixturePairFail(workspaceRoot);
    const failReport = runTerminationAbGate({
      root: workspaceRoot,
      baselinePath: failFixture.baselinePath,
      candidatePath: failFixture.candidatePath,
      minOverlapSamples: 10,
      maxOverlapMismatchRate: 0.01,
      maxDistributionDrift: 0.05,
      maxShadowConsistencyDrop: 0.01,
    });
    assert(failReport.gate.pass === false, '高偏差样本应被拦截');
    assert(
      failReport.gate.failedReasons.some(reason => reason.includes('overlap_mismatch_rate_exceeded')),
      `失败原因应包含 mismatch 拦截: ${failReport.gate.failedReasons.join('; ')}`,
    );

    console.log('\n=== termination ab gate regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'low-drift-pass',
        'high-drift-block',
      ],
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

try {
  main();
} catch (error) {
  console.error('termination ab gate 回归失败:', error?.stack || error);
  process.exit(1);
}
