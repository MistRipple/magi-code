#!/usr/bin/env node
/**
 * OpenAI 协议路由与最小测试请求回归
 *
 * 目标：
 * 1) openaiProtocol='chat' 必须映射到 chat-completions
 * 2) 配置测试必须使用最小化消息发送，不再依赖 response.content 判定成功
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
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

function verifySourceGuardrails() {
  const handlerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'handlers', 'config-handler.ts'),
    'utf8',
  );

  const minimalRequestCount = (handlerSource.match(/content: 'ping'/g) || []).length;
  const tokenOneCount = (handlerSource.match(/maxTokens: 1/g) || []).length;
  const noContentGate = !handlerSource.includes('response && response.content');

  assert(minimalRequestCount >= 3, '配置测试未统一使用最小化 ping 请求');
  assert(tokenOneCount >= 3, '配置测试未统一收紧到 maxTokens=1');
  assert(noContentGate, '配置测试仍然依赖 response.content 判定成功');
}

async function main() {
  const registryPath = path.join(OUT, 'llm', 'protocol', 'capability-registry.js');
  ensureCompiled(registryPath);

  verifySourceGuardrails();

  const { resolveProtocolId } = require(registryPath);
  assert(
    resolveProtocolId('openai', 'chat') === 'openai.chat-completions',
    'openai chat 协议未路由到 chat-completions',
  );
  assert(
    resolveProtocolId('openai', 'responses') === 'openai.responses',
    'openai responses 协议未路由到 responses',
  );
  assert(
    resolveProtocolId('anthropic') === 'anthropic.messages',
    'anthropic 协议路由异常',
  );

  console.log('\n=== openai protocol routing regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'openai_chat_protocol_routes_to_chat_completions',
      'config_test_uses_minimal_ping_request',
      'config_test_does_not_require_response_content',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('openai protocol routing 回归失败:', error?.stack || error);
  process.exit(1);
});
