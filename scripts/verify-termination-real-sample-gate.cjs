#!/usr/bin/env node
/**
 * 终止策略真实样本闸门
 *
 * 目标：
 * 1) 确保发布闸门基于真实样本（非 seed）
 * 2) 在真实样本上执行严格 AB 偏差校验
 *
 * 默认输入：
 * - baseline: .magi/metrics/termination-baseline.jsonl
 * - candidate: .magi/metrics/termination.jsonl
 */

const fs = require('fs');
const path = require('path');
const { runTerminationAbGate } = require('./verify-termination-ab-gate.cjs');

function resolveNumericOption(options, optionKey, envKey, fallback) {
  const optionValue = options[optionKey];
  if (optionValue !== undefined && optionValue !== null && optionValue !== '') {
    const parsed = Number(optionValue);
    if (!Number.isFinite(parsed)) {
      throw new Error(`${optionKey} 配置无效: ${optionValue}`);
    }
    return parsed;
  }

  const envValue = process.env[envKey];
  if (envValue !== undefined && envValue !== null && envValue !== '') {
    const parsed = Number(envValue);
    if (!Number.isFinite(parsed)) {
      throw new Error(`${envKey} 环境变量无效: ${envValue}`);
    }
    return parsed;
  }

  return fallback;
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function parseJsonl(filePath) {
  const content = fs.readFileSync(filePath, 'utf8');
  return content
    .split('\n')
    .map(line => line.trim())
    .filter(Boolean)
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`${filePath} 第 ${index + 1} 行解析失败: ${error?.message || error}`);
      }
    });
}

function isSeedRecord(record) {
  const sessionId = String(record.session_id || '');
  const planId = String(record.plan_id || '');
  return sessionId.startsWith('seed-session-') || planId.startsWith('seed-plan-');
}

function calcSeedRatio(records) {
  if (!records.length) {
    return 1;
  }
  const seedCount = records.filter(isSeedRecord).length;
  return seedCount / records.length;
}

function runTerminationRealSampleGate(options = {}) {
  const root = options.root || path.resolve(__dirname, '..');
  const metricsDir = path.join(root, '.magi', 'metrics');
  const baselinePath = options.baselinePath
    || process.env.MAGI_TERMINATION_BASELINE_PATH
    || path.join(metricsDir, 'termination-baseline.jsonl');
  const candidatePath = options.candidatePath
    || process.env.MAGI_TERMINATION_CANDIDATE_PATH
    || path.join(metricsDir, 'termination.jsonl');

  const minRealSamples = resolveNumericOption(options, 'minRealSamples', 'MAGI_REAL_GATE_MIN_SAMPLES', 100);
  const maxSeedRatio = resolveNumericOption(options, 'maxSeedRatio', 'MAGI_REAL_GATE_MAX_SEED_RATIO', 0.01);
  const maxOverlapMismatchRate = resolveNumericOption(options, 'maxOverlapMismatchRate', 'MAGI_REAL_GATE_MAX_OVERLAP_MISMATCH', 0.005);
  const maxDistributionDrift = resolveNumericOption(options, 'maxDistributionDrift', 'MAGI_REAL_GATE_MAX_DISTRIBUTION_DRIFT', 0.03);
  const maxShadowDrop = resolveNumericOption(options, 'maxShadowDrop', 'MAGI_REAL_GATE_MAX_SHADOW_DROP', 0.005);

  assert(minRealSamples > 0, `minRealSamples 必须大于 0，当前: ${minRealSamples}`);
  assert(maxSeedRatio >= 0 && maxSeedRatio <= 1, `maxSeedRatio 必须在 [0,1]，当前: ${maxSeedRatio}`);
  assert(maxOverlapMismatchRate >= 0 && maxOverlapMismatchRate <= 1, `maxOverlapMismatchRate 必须在 [0,1]，当前: ${maxOverlapMismatchRate}`);
  assert(maxDistributionDrift >= 0 && maxDistributionDrift <= 1, `maxDistributionDrift 必须在 [0,1]，当前: ${maxDistributionDrift}`);
  assert(maxShadowDrop >= 0 && maxShadowDrop <= 1, `maxShadowDrop 必须在 [0,1]，当前: ${maxShadowDrop}`);

  assert(fs.existsSync(baselinePath), `基线文件不存在: ${baselinePath}`);
  assert(fs.existsSync(candidatePath), `候选文件不存在: ${candidatePath}`);

  const baselineRecords = parseJsonl(baselinePath);
  const candidateRecords = parseJsonl(candidatePath);

  assert(baselineRecords.length >= minRealSamples, `真实样本不足（baseline）: ${baselineRecords.length} < ${minRealSamples}`);
  assert(candidateRecords.length >= minRealSamples, `真实样本不足（candidate）: ${candidateRecords.length} < ${minRealSamples}`);

  const baselineSeedRatio = calcSeedRatio(baselineRecords);
  const candidateSeedRatio = calcSeedRatio(candidateRecords);
  assert(baselineSeedRatio <= maxSeedRatio, `baseline 含 seed 样本比例过高: ${baselineSeedRatio.toFixed(4)} > ${maxSeedRatio}`);
  assert(candidateSeedRatio <= maxSeedRatio, `candidate 含 seed 样本比例过高: ${candidateSeedRatio.toFixed(4)} > ${maxSeedRatio}`);

  const report = runTerminationAbGate({
    root,
    baselinePath,
    candidatePath,
    minOverlapSamples: Math.min(baselineRecords.length, candidateRecords.length, 100),
    maxOverlapMismatchRate,
    maxDistributionDrift,
    maxShadowConsistencyDrop: maxShadowDrop,
  });
  assert(report.gate.pass === true, `真实样本 AB 闸门未通过: ${report.gate.failedReasons.join('; ')}`);

  return {
    pass: true,
    baselinePath,
    candidatePath,
    baselineSamples: baselineRecords.length,
    candidateSamples: candidateRecords.length,
    baselineSeedRatio: Number(baselineSeedRatio.toFixed(6)),
    candidateSeedRatio: Number(candidateSeedRatio.toFixed(6)),
    overlapMismatchRate: report.overlap.mismatchRate,
    distributionDrift: report.reasonDistributionDrift.value,
    shadowDrop: report.shadow.drop,
  };
}

function main() {
  const result = runTerminationRealSampleGate();
  console.log('\n=== termination real sample gate ===');
  console.log(JSON.stringify(result, null, 2));
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error('termination real sample gate 失败:', error?.stack || error);
    process.exit(1);
  }
}

module.exports = {
  runTerminationRealSampleGate,
};
