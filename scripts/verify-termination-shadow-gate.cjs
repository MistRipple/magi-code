#!/usr/bin/env node
/**
 * Shadow 一致率发布闸门
 *
 * 默认阈值：
 * - consistency >= 0.99
 * - min samples >= 20
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const METRICS_PATH = path.join(ROOT, '.magi', 'metrics', 'termination.jsonl');

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
        throw new Error(`termination.jsonl 第 ${index + 1} 行解析失败: ${error?.message || error}`);
      }
    });
}

function main() {
  const threshold = Number(process.env.MAGI_SHADOW_GATE_THRESHOLD || '0.99');
  const minSamples = Number(process.env.MAGI_SHADOW_GATE_MIN_SAMPLES || '20');
  assert(Number.isFinite(threshold) && threshold > 0 && threshold <= 1, `无效阈值: ${threshold}`);
  assert(Number.isFinite(minSamples) && minSamples > 0, `无效样本阈值: ${minSamples}`);

  assert(fs.existsSync(METRICS_PATH), `未找到 termination.jsonl: ${METRICS_PATH}`);
  const records = parseJsonl(METRICS_PATH);
  const shadowRecords = records.filter(record => record.shadow && record.shadow.enabled === true);
  assert(shadowRecords.length >= minSamples, `shadow 样本不足: ${shadowRecords.length} < ${minSamples}`);

  const consistentCount = shadowRecords.filter(record => record.shadow.consistent === true).length;
  const rate = consistentCount / shadowRecords.length;
  assert(rate >= threshold, `shadow 一致率未达标: ${rate.toFixed(4)} < ${threshold}`);

  console.log('\n=== termination shadow gate ===');
  console.log(JSON.stringify({
    pass: true,
    threshold,
    minSamples,
    samples: shadowRecords.length,
    consistent: consistentCount,
    consistencyRate: Number(rate.toFixed(4)),
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('termination shadow gate 失败:', error?.stack || error);
  process.exit(1);
}
