#!/usr/bin/env node
/**
 * 终止指标离线重放脚本（骨架）
 *
 * 目标：
 * 1) 验证 termination.jsonl 的字段完整性
 * 2) 计算 reason 分布与 shadow 一致率
 * 3) 作为后续新旧策略比对的入口
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

function parseJsonl(content) {
  return content
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line, idx) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`第 ${idx + 1} 行 JSON 解析失败: ${error?.message || error}`);
      }
    });
}

function fallbackRecords() {
  return [{
    timestamp: new Date().toISOString(),
    session_id: 'fallback-session',
    plan_id: 'fallback-plan',
    turn_id: 'fallback-turn',
    mode: 'standard',
    final_status: 'completed',
    reason: 'completed',
    rounds: 1,
    duration_ms: 1000,
    token_used: 120,
    evidence_ids: ['fallback-evidence'],
    progress_vector: {
      terminalRequiredTodos: 1,
      acceptedCriteria: 1,
      criticalPathResolved: 1,
      unresolvedBlockers: 0,
    },
    review_state: {
      accepted: 1,
      total: 1,
    },
    blocker_state: {
      open: 0,
      score: 0,
      externalWaitOpen: 0,
      maxExternalWaitAgeMs: 0,
    },
    budget_state: {
      elapsedMs: 1000,
      tokenUsed: 120,
      errorRate: 0,
    },
    required_total: 1,
    failed_required: 0,
    running_or_pending_required: 0,
    shadow: {
      enabled: true,
      reason: 'completed',
      consistent: true,
    },
  }];
}

function validateRecord(record, index) {
  const required = ['timestamp', 'session_id', 'mode', 'reason', 'duration_ms', 'token_used'];
  for (const key of required) {
    assert(record[key] !== undefined && record[key] !== null, `记录 ${index + 1} 缺少字段 ${key}`);
  }
  assert(typeof record.reason === 'string' && record.reason.length > 0, `记录 ${index + 1} reason 非法`);
  assert(Number.isFinite(record.duration_ms) && record.duration_ms >= 0, `记录 ${index + 1} duration_ms 非法`);
  assert(Number.isFinite(record.token_used) && record.token_used >= 0, `记录 ${index + 1} token_used 非法`);

  if (record.progress_vector != null) {
    const pv = record.progress_vector;
    assert(Number.isFinite(pv.terminalRequiredTodos), `记录 ${index + 1} progress_vector.terminalRequiredTodos 非法`);
    assert(Number.isFinite(pv.acceptedCriteria), `记录 ${index + 1} progress_vector.acceptedCriteria 非法`);
    assert(Number.isFinite(pv.criticalPathResolved), `记录 ${index + 1} progress_vector.criticalPathResolved 非法`);
    assert(Number.isFinite(pv.unresolvedBlockers), `记录 ${index + 1} progress_vector.unresolvedBlockers 非法`);
  }
}

function summarize(records) {
  const reasonDistribution = {};
  let shadowEnabled = 0;
  let shadowConsistent = 0;

  for (const record of records) {
    reasonDistribution[record.reason] = (reasonDistribution[record.reason] || 0) + 1;
    if (record.shadow && record.shadow.enabled) {
      shadowEnabled += 1;
      if (record.shadow.consistent === true) {
        shadowConsistent += 1;
      }
    }
  }

  return {
    total: records.length,
    reasonDistribution,
    shadowEnabled,
    shadowConsistent,
    shadowConsistencyRate: shadowEnabled > 0 ? Number((shadowConsistent / shadowEnabled).toFixed(4)) : null,
  };
}

function main() {
  let records = [];
  let dataSource = METRICS_PATH;

  if (fs.existsSync(METRICS_PATH)) {
    const content = fs.readFileSync(METRICS_PATH, 'utf8');
    records = parseJsonl(content);
  } else {
    records = fallbackRecords();
    dataSource = 'fallback-fixture';
  }

  assert(records.length > 0, '无可重放的终止记录');
  records.forEach((record, idx) => validateRecord(record, idx));
  const summary = summarize(records);

  console.log('\n=== termination replay ===');
  console.log(JSON.stringify({
    pass: true,
    dataSource,
    ...summary,
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('termination replay 失败:', error?.stack || error);
  process.exit(1);
}
