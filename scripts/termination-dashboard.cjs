#!/usr/bin/env node
/**
 * 终止指标聚合脚本
 *
 * 产出：
 * - .magi/metrics/termination-dashboard.json
 * - .magi/metrics/termination-dashboard.md
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const METRICS_DIR = path.join(ROOT, '.magi', 'metrics');
const INPUT_PATH = path.join(METRICS_DIR, 'termination.jsonl');
const OUTPUT_JSON = path.join(METRICS_DIR, 'termination-dashboard.json');
const OUTPUT_MD = path.join(METRICS_DIR, 'termination-dashboard.md');

function parseRecords(content) {
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

function safeAverage(values) {
  if (!values.length) {
    return 0;
  }
  const sum = values.reduce((acc, value) => acc + value, 0);
  return sum / values.length;
}

function buildSummary(records) {
  const reasonDistribution = {};
  const statusDistribution = {};
  const durationValues = [];
  const tokenValues = [];
  let shadowEnabled = 0;
  let shadowConsistent = 0;

  for (const record of records) {
    const reason = String(record.reason || 'unknown');
    const status = String(record.final_status || 'unknown');
    reasonDistribution[reason] = (reasonDistribution[reason] || 0) + 1;
    statusDistribution[status] = (statusDistribution[status] || 0) + 1;

    if (Number.isFinite(record.duration_ms)) {
      durationValues.push(Number(record.duration_ms));
    }
    if (Number.isFinite(record.token_used)) {
      tokenValues.push(Number(record.token_used));
    }

    if (record.shadow && record.shadow.enabled === true) {
      shadowEnabled += 1;
      if (record.shadow.consistent === true) {
        shadowConsistent += 1;
      }
    }
  }

  const shadowConsistencyRate = shadowEnabled > 0 ? shadowConsistent / shadowEnabled : null;

  return {
    generatedAt: new Date().toISOString(),
    total: records.length,
    reasonDistribution,
    statusDistribution,
    avgDurationMs: Number(safeAverage(durationValues).toFixed(2)),
    avgTokenUsed: Number(safeAverage(tokenValues).toFixed(2)),
    shadowEnabled,
    shadowConsistent,
    shadowConsistencyRate: shadowConsistencyRate == null ? null : Number(shadowConsistencyRate.toFixed(4)),
  };
}

function toMarkdown(summary) {
  const reasonRows = Object.entries(summary.reasonDistribution)
    .sort((a, b) => b[1] - a[1])
    .map(([reason, count]) => `| ${reason} | ${count} |`)
    .join('\n') || '| (none) | 0 |';

  const statusRows = Object.entries(summary.statusDistribution)
    .sort((a, b) => b[1] - a[1])
    .map(([status, count]) => `| ${status} | ${count} |`)
    .join('\n') || '| (none) | 0 |';

  return [
    '# Termination Dashboard',
    '',
    `- Generated At: ${summary.generatedAt}`,
    `- Total Samples: ${summary.total}`,
    `- Avg Duration(ms): ${summary.avgDurationMs}`,
    `- Avg Token Used: ${summary.avgTokenUsed}`,
    `- Shadow Enabled: ${summary.shadowEnabled}`,
    `- Shadow Consistency Rate: ${summary.shadowConsistencyRate == null ? 'N/A' : summary.shadowConsistencyRate}`,
    '',
    '## Reason Distribution',
    '| Reason | Count |',
    '|---|---|',
    reasonRows,
    '',
    '## Final Status Distribution',
    '| Status | Count |',
    '|---|---|',
    statusRows,
    '',
  ].join('\n');
}

function main() {
  if (!fs.existsSync(INPUT_PATH)) {
    throw new Error(`未找到终止指标文件: ${INPUT_PATH}`);
  }
  const records = parseRecords(fs.readFileSync(INPUT_PATH, 'utf8'));
  if (records.length === 0) {
    throw new Error('termination.jsonl 为空，无法生成 dashboard');
  }

  const summary = buildSummary(records);
  fs.mkdirSync(METRICS_DIR, { recursive: true });
  fs.writeFileSync(OUTPUT_JSON, JSON.stringify(summary, null, 2), 'utf8');
  fs.writeFileSync(OUTPUT_MD, toMarkdown(summary), 'utf8');

  console.log('\n=== termination dashboard ===');
  console.log(JSON.stringify({
    pass: true,
    total: summary.total,
    outputJson: OUTPUT_JSON,
    outputMd: OUTPUT_MD,
    shadowConsistencyRate: summary.shadowConsistencyRate,
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('termination dashboard 生成失败:', error?.stack || error);
  process.exit(1);
}
