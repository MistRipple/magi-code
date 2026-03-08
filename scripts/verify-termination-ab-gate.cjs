#!/usr/bin/env node
/**
 * 终止策略 A/B 偏差率闸门
 *
 * 输入：
 * - baseline: MAGI_TERMINATION_BASELINE_PATH（默认 .magi/metrics/termination-baseline.jsonl）
 * - candidate: MAGI_TERMINATION_CANDIDATE_PATH（默认 .magi/metrics/termination.jsonl）
 *
 * 输出：
 * - .magi/metrics/termination-ab-diff.json
 * - .magi/metrics/termination-ab-diff.md
 */

const fs = require('fs');
const path = require('path');

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

function normalizeReasonDistribution(records) {
  const distribution = {};
  for (const record of records) {
    const reason = String(record.reason || 'unknown');
    distribution[reason] = (distribution[reason] || 0) + 1;
  }
  const total = records.length || 1;
  for (const key of Object.keys(distribution)) {
    distribution[key] = distribution[key] / total;
  }
  return distribution;
}

function calcTotalVariationDistance(baselineDist, candidateDist) {
  const keys = new Set([...Object.keys(baselineDist), ...Object.keys(candidateDist)]);
  let sum = 0;
  for (const key of keys) {
    const p = baselineDist[key] || 0;
    const q = candidateDist[key] || 0;
    sum += Math.abs(p - q);
  }
  return sum / 2;
}

function buildRecordKey(record) {
  const sessionId = String(record.session_id || '');
  const planId = String(record.plan_id || '');
  const turnId = String(record.turn_id || '');
  const rounds = Number.isFinite(record.rounds) ? Number(record.rounds) : 0;
  return `${sessionId}::${planId}::${turnId}::${rounds}`;
}

function calcOverlapMismatchRate(baselineRecords, candidateRecords) {
  const baselineMap = new Map();
  for (const record of baselineRecords) {
    baselineMap.set(buildRecordKey(record), record);
  }

  let overlap = 0;
  let mismatched = 0;
  for (const record of candidateRecords) {
    const key = buildRecordKey(record);
    const baseline = baselineMap.get(key);
    if (!baseline) {
      continue;
    }
    overlap += 1;
    const a = String(baseline.reason || 'unknown');
    const b = String(record.reason || 'unknown');
    if (a !== b) {
      mismatched += 1;
    }
  }

  return {
    overlap,
    mismatched,
    mismatchRate: overlap > 0 ? mismatched / overlap : null,
  };
}

function calcShadowConsistencyRate(records) {
  const shadowRecords = records.filter(record => record.shadow && record.shadow.enabled === true);
  if (shadowRecords.length === 0) {
    return null;
  }
  const consistent = shadowRecords.filter(record => record.shadow.consistent === true).length;
  return consistent / shadowRecords.length;
}

function toMarkdown(report) {
  const reasonRows = Object.entries(report.reasonDistributionDrift.details)
    .sort((a, b) => b[1].absDiff - a[1].absDiff)
    .map(([reason, item]) =>
      `| ${reason} | ${item.baseline.toFixed(4)} | ${item.candidate.toFixed(4)} | ${item.absDiff.toFixed(4)} |`)
    .join('\n') || '| (none) | 0 | 0 | 0 |';

  return [
    '# Termination AB Gate Report',
    '',
    `- Generated At: ${report.generatedAt}`,
    `- Baseline Path: ${report.baseline.path}`,
    `- Candidate Path: ${report.candidate.path}`,
    `- Baseline Samples: ${report.baseline.samples}`,
    `- Candidate Samples: ${report.candidate.samples}`,
    '',
    '## Gate Threshold',
    `- minOverlapSamples: ${report.threshold.minOverlapSamples}`,
    `- maxOverlapMismatchRate: ${report.threshold.maxOverlapMismatchRate}`,
    `- maxDistributionDrift: ${report.threshold.maxDistributionDrift}`,
    `- maxShadowConsistencyDrop: ${report.threshold.maxShadowConsistencyDrop}`,
    '',
    '## Gate Result',
    `- pass: ${report.gate.pass}`,
    `- mode: ${report.gate.mode}`,
    `- failedReasons: ${report.gate.failedReasons.length > 0 ? report.gate.failedReasons.join('; ') : 'none'}`,
    '',
    '## Metrics',
    `- overlapSamples: ${report.overlap.overlap}`,
    `- overlapMismatchRate: ${report.overlap.mismatchRate == null ? 'N/A' : report.overlap.mismatchRate.toFixed(4)}`,
    `- distributionDrift(TVD): ${report.reasonDistributionDrift.value.toFixed(4)}`,
    `- baselineShadowConsistency: ${report.shadow.baseline == null ? 'N/A' : report.shadow.baseline.toFixed(4)}`,
    `- candidateShadowConsistency: ${report.shadow.candidate == null ? 'N/A' : report.shadow.candidate.toFixed(4)}`,
    `- shadowConsistencyDrop: ${report.shadow.drop == null ? 'N/A' : report.shadow.drop.toFixed(4)}`,
    '',
    '## Reason Drift Detail',
    '| Reason | Baseline | Candidate | |Δ| |',
    '|---|---:|---:|---:|',
    reasonRows,
    '',
  ].join('\n');
}

function buildReasonDiffDetails(baselineDist, candidateDist) {
  const keys = new Set([...Object.keys(baselineDist), ...Object.keys(candidateDist)]);
  const details = {};
  for (const key of keys) {
    const baseline = baselineDist[key] || 0;
    const candidate = candidateDist[key] || 0;
    details[key] = {
      baseline,
      candidate,
      absDiff: Math.abs(baseline - candidate),
    };
  }
  return details;
}

function runTerminationAbGate(options = {}) {
  const root = options.root || path.resolve(__dirname, '..');
  const metricsDir = path.join(root, '.magi', 'metrics');
  const baselinePath = options.baselinePath
    || process.env.MAGI_TERMINATION_BASELINE_PATH
    || path.join(metricsDir, 'termination-baseline.jsonl');
  const candidatePath = options.candidatePath
    || process.env.MAGI_TERMINATION_CANDIDATE_PATH
    || path.join(metricsDir, 'termination.jsonl');

  const minOverlapSamples = resolveNumericOption(options, 'minOverlapSamples', 'MAGI_AB_GATE_MIN_OVERLAP_SAMPLES', 20);
  const maxOverlapMismatchRate = resolveNumericOption(options, 'maxOverlapMismatchRate', 'MAGI_AB_GATE_MAX_OVERLAP_MISMATCH', 0.01);
  const maxDistributionDrift = resolveNumericOption(options, 'maxDistributionDrift', 'MAGI_AB_GATE_MAX_DISTRIBUTION_DRIFT', 0.05);
  const maxShadowConsistencyDrop = resolveNumericOption(options, 'maxShadowConsistencyDrop', 'MAGI_AB_GATE_MAX_SHADOW_DROP', 0.01);

  if (!(minOverlapSamples > 0)) {
    throw new Error(`minOverlapSamples 必须大于 0，当前: ${minOverlapSamples}`);
  }
  if (!(maxOverlapMismatchRate >= 0 && maxOverlapMismatchRate <= 1)) {
    throw new Error(`maxOverlapMismatchRate 必须在 [0,1]，当前: ${maxOverlapMismatchRate}`);
  }
  if (!(maxDistributionDrift >= 0 && maxDistributionDrift <= 1)) {
    throw new Error(`maxDistributionDrift 必须在 [0,1]，当前: ${maxDistributionDrift}`);
  }
  if (!(maxShadowConsistencyDrop >= 0 && maxShadowConsistencyDrop <= 1)) {
    throw new Error(`maxShadowConsistencyDrop 必须在 [0,1]，当前: ${maxShadowConsistencyDrop}`);
  }

  if (!fs.existsSync(baselinePath)) {
    throw new Error(`基线文件不存在: ${baselinePath}`);
  }
  if (!fs.existsSync(candidatePath)) {
    throw new Error(`候选文件不存在: ${candidatePath}`);
  }

  const baselineRecords = parseJsonl(baselinePath);
  const candidateRecords = parseJsonl(candidatePath);
  if (baselineRecords.length === 0) {
    throw new Error('基线样本为空');
  }
  if (candidateRecords.length === 0) {
    throw new Error('候选样本为空');
  }

  const baselineDist = normalizeReasonDistribution(baselineRecords);
  const candidateDist = normalizeReasonDistribution(candidateRecords);
  const drift = calcTotalVariationDistance(baselineDist, candidateDist);
  const overlap = calcOverlapMismatchRate(baselineRecords, candidateRecords);

  const baselineShadow = calcShadowConsistencyRate(baselineRecords);
  const candidateShadow = calcShadowConsistencyRate(candidateRecords);
  const shadowDrop = (baselineShadow == null || candidateShadow == null)
    ? null
    : baselineShadow - candidateShadow;

  const failedReasons = [];
  let gateMode = 'distribution';

  if (overlap.overlap >= minOverlapSamples) {
    gateMode = 'overlap';
    if (overlap.mismatchRate == null || overlap.mismatchRate > maxOverlapMismatchRate) {
      failedReasons.push(`overlap_mismatch_rate_exceeded(${overlap.mismatchRate == null ? 'null' : overlap.mismatchRate.toFixed(4)} > ${maxOverlapMismatchRate})`);
    }
  } else if (drift > maxDistributionDrift) {
    failedReasons.push(`distribution_drift_exceeded(${drift.toFixed(4)} > ${maxDistributionDrift})`);
  }

  if (shadowDrop != null && shadowDrop > maxShadowConsistencyDrop) {
    failedReasons.push(`shadow_consistency_drop_exceeded(${shadowDrop.toFixed(4)} > ${maxShadowConsistencyDrop})`);
  }

  const report = {
    generatedAt: new Date().toISOString(),
    baseline: {
      path: baselinePath,
      samples: baselineRecords.length,
    },
    candidate: {
      path: candidatePath,
      samples: candidateRecords.length,
    },
    threshold: {
      minOverlapSamples,
      maxOverlapMismatchRate,
      maxDistributionDrift,
      maxShadowConsistencyDrop,
    },
    overlap,
    reasonDistributionDrift: {
      value: Number(drift.toFixed(6)),
      details: buildReasonDiffDetails(baselineDist, candidateDist),
    },
    shadow: {
      baseline: baselineShadow == null ? null : Number(baselineShadow.toFixed(6)),
      candidate: candidateShadow == null ? null : Number(candidateShadow.toFixed(6)),
      drop: shadowDrop == null ? null : Number(shadowDrop.toFixed(6)),
    },
    gate: {
      pass: failedReasons.length === 0,
      mode: gateMode,
      failedReasons,
    },
  };

  fs.mkdirSync(metricsDir, { recursive: true });
  fs.writeFileSync(path.join(metricsDir, 'termination-ab-diff.json'), JSON.stringify(report, null, 2), 'utf8');
  fs.writeFileSync(path.join(metricsDir, 'termination-ab-diff.md'), toMarkdown(report), 'utf8');

  return report;
}

function main() {
  const report = runTerminationAbGate();
  console.log('\n=== termination ab gate ===');
  console.log(JSON.stringify({
    pass: report.gate.pass,
    mode: report.gate.mode,
    failedReasons: report.gate.failedReasons,
    overlapSamples: report.overlap.overlap,
    overlapMismatchRate: report.overlap.mismatchRate,
    distributionDrift: report.reasonDistributionDrift.value,
    shadowDrop: report.shadow.drop,
  }, null, 2));
  if (!report.gate.pass) {
    process.exit(1);
  }
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error('termination ab gate 失败:', error?.stack || error);
    process.exit(1);
  }
}

module.exports = {
  runTerminationAbGate,
};
