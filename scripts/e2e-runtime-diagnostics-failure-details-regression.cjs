#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function readSource(relPath) {
  return fs.readFileSync(path.join(ROOT, relPath), 'utf8');
}

function main() {
  const providerSource = readSource(path.join('src', 'ui', 'webview-provider.ts'));
  const messageTypeSource = readSource(path.join('src', 'ui', 'webview-svelte', 'src', 'types', 'message.ts'));
  const storeSource = readSource(path.join('src', 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.ts'));
  const handlerSource = readSource(path.join('src', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.ts'));
  const panelSource = readSource(path.join('src', 'ui', 'webview-svelte', 'src', 'components', 'RuntimeDiagnosticsPanel.svelte'));

  assert(
    providerSource.includes('private async enrichExecutionFailureContext('),
    'WebviewProvider 未收集失败子任务上下文',
  );
  assert(
    providerSource.includes('failureReason: input.result.failureReason || undefined'),
    '运行态诊断消息未透出 failureReason',
  );
  assert(
    providerSource.includes('errors: input.result.errors'),
    '运行态诊断消息未透出 errors',
  );
  assert(
    messageTypeSource.includes('failureReason?: string;') && messageTypeSource.includes('errors?: string[];'),
    '前端运行态诊断类型未声明 failureReason/errors',
  );
  assert(
    storeSource.includes('const failureReason = typeof input.failureReason === \'string\''),
    'messages store 未保留 failureReason',
  );
  assert(
    storeSource.includes('const errors = Array.isArray(input.errors)'),
    'messages store 未保留 errors',
  );
  assert(
    handlerSource.includes('typeof message.failureReason === \'string\''),
    'message-handler 未接收 failureReason',
  );
  assert(
    handlerSource.includes('Array.isArray(message.errors)'),
    'message-handler 未接收 errors',
  );
  assert(
    panelSource.includes("i18n.t('runtimeDiagnostics.failureTitle')"),
    'RuntimeDiagnosticsPanel 未渲染失败详情标题',
  );
  assert(
    panelSource.includes('runtime-diagnostics__failure-list'),
    'RuntimeDiagnosticsPanel 未渲染失败错误列表',
  );

  console.log('\n=== runtime diagnostics failure details regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'provider_enriches_failure_context',
      'runtime_diagnostics_message_includes_failure_reason_and_errors',
      'frontend_store_preserves_failure_details',
      'runtime_panel_renders_failure_details',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('runtime diagnostics failure details 回归失败:', error?.stack || error);
  process.exit(1);
}
