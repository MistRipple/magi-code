#!/usr/bin/env node
/**
 * 对话连续性门禁回归脚本
 *
 * 目标：
 * 1) MissionDrivenEngine 不再因门禁状态抛出异常中断会话（fail-open）
 * 2) 上游模型错误与 Phase C 校验异常会降级为可读返回文本
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function main() {
  const filePath = path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts');
  const source = fs.readFileSync(filePath, 'utf8');

  assert(
    source.includes('const executionWarnings: string[] = [];'),
    '缺少 executionWarnings 门禁降级聚合逻辑'
  );
  assert(
    source.includes('编排器.统一执行.上游模型异常_已降级'),
    '缺少上游模型异常降级日志'
  );
  assert(
    source.includes('编排器.PhaseC.校验异常_已降级'),
    '缺少 Phase C 校验异常降级日志'
  );
  assert(
    source.includes('[System] 本轮触发门禁降级（会话不中断）：'),
    '缺少门禁降级用户可见提示'
  );
  assert(
    source.includes('const degradedMessage = `[System] 本轮执行出现异常，已自动降级为不中断返回：${errorMessage}`;'),
    '缺少 catch fail-open 降级返回'
  );

  const catchRegionRegex = /} catch \(error\) \{[\s\S]{0,1200}const degradedMessage = `\[System\] 本轮执行出现异常，已自动降级为不中断返回：\$\{errorMessage\}`;[\s\S]{0,200}return degradedMessage;[\s\S]{0,80}\} finally \{/;
  assert(
    catchRegionRegex.test(source),
    '统一执行 catch 仍可能直接抛错，未满足对话连续性要求'
  );

  console.log('\n=== conversation continuity gate regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'gate-fail-open-warnings',
      'phase-c-verification-degrade',
      'catch-fail-open-return',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('conversation continuity gate 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}
