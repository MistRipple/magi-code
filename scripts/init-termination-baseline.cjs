#!/usr/bin/env node
/**
 * 初始化终止策略 A/B 基线文件
 *
 * 默认行为：
 * - 从 .magi/metrics/termination.jsonl 读取候选样本
 * - 输出为 .magi/metrics/termination-baseline.jsonl
 * - 默认要求最少 20 条样本
 * - 若基线已存在，默认拒绝覆盖（可通过 MAGI_TERMINATION_BASELINE_OVERWRITE=1 强制覆盖）
 */

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function main() {
  const root = path.resolve(__dirname, '..');
  const metricsDir = path.join(root, '.magi', 'metrics');

  const candidatePath = process.env.MAGI_TERMINATION_CANDIDATE_PATH
    || path.join(metricsDir, 'termination.jsonl');
  const baselinePath = process.env.MAGI_TERMINATION_BASELINE_PATH
    || path.join(metricsDir, 'termination-baseline.jsonl');
  const metaPath = path.join(metricsDir, 'termination-baseline.meta.json');

  const minSamples = Number(process.env.MAGI_TERMINATION_BASELINE_MIN_SAMPLES || '20');
  const overwrite = process.env.MAGI_TERMINATION_BASELINE_OVERWRITE === '1';

  assert(Number.isFinite(minSamples) && minSamples > 0, `无效最小样本阈值: ${minSamples}`);
  assert(fs.existsSync(candidatePath), `候选样本文件不存在: ${candidatePath}`);

  const raw = fs.readFileSync(candidatePath, 'utf8');
  const lines = raw
    .split('\n')
    .map(line => line.trim())
    .filter(Boolean);
  assert(lines.length >= minSamples, `候选样本不足: ${lines.length} < ${minSamples}`);

  if (fs.existsSync(baselinePath) && !overwrite) {
    throw new Error(`基线文件已存在，拒绝覆盖: ${baselinePath}（如需覆盖，请设置 MAGI_TERMINATION_BASELINE_OVERWRITE=1）`);
  }

  fs.mkdirSync(metricsDir, { recursive: true });
  const baselineRaw = `${lines.join('\n')}\n`;
  fs.writeFileSync(baselinePath, baselineRaw, 'utf8');

  const digest = crypto.createHash('sha256').update(baselineRaw).digest('hex');
  const meta = {
    createdAt: new Date().toISOString(),
    candidatePath,
    baselinePath,
    samples: lines.length,
    minSamples,
    sha256: digest,
  };
  fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2), 'utf8');

  console.log('\n=== init termination baseline ===');
  console.log(JSON.stringify({
    pass: true,
    baselinePath,
    metaPath,
    samples: lines.length,
    minSamples,
    overwrite,
    sha256: digest,
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('init termination baseline 失败:', error?.stack || error);
  process.exit(1);
}
