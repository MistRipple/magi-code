#!/usr/bin/env node
/**
 * termination real sample gate 回归脚本
 *
 * 覆盖目标：
 * 1) seed 样本必须被拦截
 * 2) 非 seed 样本在低偏差下应通过
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const { runTerminationRealSampleGate } = require('./verify-termination-real-sample-gate.cjs');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function writeJsonl(filePath, records) {
  fs.writeFileSync(filePath, `${records.map(r => JSON.stringify(r)).join('\n')}\n`, 'utf8');
}

function buildRecord(seed, reason, options = {}) {
  const real = options.real === true;
  return {
    timestamp: new Date(Date.now() + seed * 1000).toISOString(),
    session_id: real ? `real-session-${seed}` : `seed-session-${seed}`,
    plan_id: real ? `real-plan-${seed}` : `seed-plan-${seed}`,
    turn_id: `${real ? 'real' : 'seed'}-turn-${seed}`,
    mode: 'standard',
    final_status: reason === 'completed' ? 'completed' : 'failed',
    reason,
    rounds: reason === 'completed' ? 2 : 4,
    duration_ms: 1000 + seed,
    token_used: 200 + seed,
    evidence_ids: [`e-${seed}`],
    progress_vector: {
      terminalRequiredTodos: reason === 'completed' ? 2 : 1,
      acceptedCriteria: reason === 'completed' ? 2 : 1,
      criticalPathResolved: reason === 'completed' ? 1 : 0.6,
      unresolvedBlockers: reason === 'completed' ? 0 : 1,
    },
    shadow: {
      enabled: true,
      reason,
      consistent: true,
    },
  };
}

function main() {
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-real-gate-'));
  try {
    const baselinePath = path.join(workspaceRoot, 'baseline.jsonl');
    const candidatePath = path.join(workspaceRoot, 'candidate.jsonl');

    const seedRecords = Array.from({ length: 20 }, (_, i) => buildRecord(i, i < 16 ? 'completed' : 'failed', { real: false }));
    writeJsonl(baselinePath, seedRecords);
    writeJsonl(candidatePath, seedRecords);

    let blocked = false;
    try {
      runTerminationRealSampleGate({
        root: workspaceRoot,
        baselinePath,
        candidatePath,
        minRealSamples: 20,
        maxSeedRatio: 0.01,
        maxOverlapMismatchRate: 0.005,
        maxDistributionDrift: 0.03,
        maxShadowDrop: 0.005,
      });
    } catch (error) {
      blocked = String(error?.message || error).includes('seed 样本比例过高');
    }
    assert(blocked, 'seed 样本应被真实样本闸门拦截');

    const realBaseline = Array.from({ length: 24 }, (_, i) => buildRecord(i, i < 20 ? 'completed' : 'failed', { real: true }));
    const realCandidate = Array.from({ length: 24 }, (_, i) => buildRecord(i, i < 20 ? 'completed' : 'failed', { real: true }));
    writeJsonl(baselinePath, realBaseline);
    writeJsonl(candidatePath, realCandidate);

    const passResult = runTerminationRealSampleGate({
      root: workspaceRoot,
      baselinePath,
      candidatePath,
      minRealSamples: 20,
      maxSeedRatio: 0.01,
      maxOverlapMismatchRate: 0.005,
      maxDistributionDrift: 0.03,
      maxShadowDrop: 0.005,
    });
    assert(passResult.pass === true, '真实样本低偏差场景应通过');

    console.log('\n=== termination real sample gate regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'seed-sample-blocked',
        'real-sample-pass',
      ],
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

try {
  main();
} catch (error) {
  console.error('termination real sample gate 回归失败:', error?.stack || error);
  process.exit(1);
}
