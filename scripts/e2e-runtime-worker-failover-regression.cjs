#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve('/Users/xie/code/magi');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function ensureCompiled(file) {
  if (!fs.existsSync(file)) {
    throw new Error(`缺少 out 编译产物: ${file}，请先执行 npm run -s compile`);
  }
}

function main() {
  const routingSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'core', 'dispatch-routing-service.ts'),
    'utf8',
  );
  const managerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'core', 'dispatch-manager.ts'),
    'utf8',
  );

  assert(
    routingSource.includes('shouldAutoFailoverRuntime(errorMessage: string): boolean'),
    '缺少运行时瞬时错误自动改派判定入口',
  );
  assert(
    /rate limit\|limit exceeded\|限流\|timeout\|timed out\|超时/.test(routingSource),
    '自动改派判定未覆盖限流/超时等瞬时基础设施错误',
  );
  assert(
    managerSource.includes('this.routingService.shouldAutoFailoverRuntime(rawErrorMsg)'),
    'dispatch-manager 未接入运行时自动改派判定',
  );
  assert(
    managerSource.includes("t('dispatch.notify.runtimeExecutionFailover'"),
    '缺少运行中自动改派用户通知',
  );
  assert(
    managerSource.includes('continue;'),
    '运行中自动改派后未继续执行后续 attempt',
  );

  const routingOut = path.join(OUT, 'orchestrator', 'core', 'dispatch-routing-service.js');
  ensureCompiled(routingOut);
  const { DispatchRoutingService } = require(routingOut);
  const stubProfileLoader = {
    getAssignmentLoader() {
      return { reload() {} };
    },
    getWorkerForCategory() {
      return 'claude';
    },
    getAllCategories() {
      return new Map([['architecture', {}]]);
    },
    getCategory() {
      return {};
    },
  };
  const service = new DispatchRoutingService(
    stubProfileLoader,
    ['claude', 'codex', 'gemini'],
    { claude: ['codex', 'gemini'], codex: ['claude', 'gemini'], gemini: ['claude', 'codex'] },
    60_000,
  );

  assert(service.shouldAutoFailoverRuntime('模型服务触发限流，请稍后重试'), '限流错误应触发自动改派');
  assert(service.shouldAutoFailoverRuntime('503 Service Unavailable'), '503 应触发自动改派');
  assert(!service.shouldAutoFailoverRuntime('invalid api key'), '鉴权错误不应触发自动改派');

  console.log('\n=== runtime worker failover regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'transient_runtime_errors_enable_failover',
      'dispatch_manager_continues_with_fallback_worker',
      'auth_errors_do_not_trigger_failover',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('runtime worker failover 回归失败:', error && error.stack ? error.stack : error);
  process.exit(1);
}
