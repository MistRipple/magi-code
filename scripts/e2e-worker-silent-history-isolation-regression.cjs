#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function extractSilentMessageBody(source) {
  const signature = 'async sendSilentMessage(message: string): Promise<string> {';
  const start = source.indexOf(signature);
  if (start < 0) {
    throw new Error('未找到 WorkerLLMAdapter.sendSilentMessage 定义');
  }

  let depth = 0;
  let bodyStart = -1;
  for (let i = start; i < source.length; i += 1) {
    const char = source[i];
    if (char === '{') {
      depth += 1;
      if (bodyStart < 0) {
        bodyStart = i + 1;
      }
    } else if (char === '}') {
      depth -= 1;
      if (depth === 0 && bodyStart >= 0) {
        return source.slice(bodyStart, i);
      }
    }
  }

  throw new Error('sendSilentMessage 方法体提取失败');
}

function main() {
  const source = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapters', 'worker-adapter.ts'),
    'utf8',
  );

  const body = extractSilentMessageBody(source);

  assert(
    body.includes('const silentHistory = [...this.conversationHistory, { role: \'user\' as const, content: message }];'),
    '静默调用未使用隔离会话历史快照',
  );
  assert(
    body.includes('messages: silentHistory,'),
    '静默调用仍未使用隔离历史发送请求',
  );
  assert(
    !body.includes('this.conversationHistory.push({ role: \'user\', content: message });'),
    '静默调用仍把内部提示写入正式用户会话历史',
  );
  assert(
    !body.includes('this.conversationHistory.push({ role: \'assistant\', content });'),
    '静默调用仍把内部结果写回正式用户会话历史',
  );

  console.log('\n=== worker silent history isolation regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'silent_message_uses_ephemeral_history_snapshot',
      'silent_message_does_not_append_user_prompt_to_visible_history',
      'silent_message_does_not_append_assistant_result_to_visible_history',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('worker silent history isolation 回归失败:', error?.stack || error);
  process.exit(1);
}
