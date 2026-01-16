#!/usr/bin/env node
/**
 * Intent Gate 测试脚本
 * 验证意图分类和门控逻辑是否正确
 */

const { IntentClassifier, IntentType } = require('../out/orchestrator/intent-classifier');
const { IntentGate, IntentHandlerMode } = require('../out/orchestrator/intent-gate');

console.log('======================================================================');
console.log('Intent Gate 测试');
console.log('======================================================================\n');

// 测试用例
const testCases = [
  // 问答类
  { prompt: '你可以做什么', expectedType: IntentType.QUESTION, expectedMode: IntentHandlerMode.ASK },
  { prompt: '什么是JWT', expectedType: IntentType.QUESTION, expectedMode: IntentHandlerMode.ASK },
  { prompt: '你好', expectedType: IntentType.QUESTION, expectedMode: IntentHandlerMode.ASK },
  { prompt: 'React和Vue有什么区别？', expectedType: IntentType.QUESTION, expectedMode: IntentHandlerMode.ASK },
  { prompt: '解释一下这段代码的作用', expectedType: IntentType.QUESTION, expectedMode: IntentHandlerMode.ASK },
  
  // 探索类
  { prompt: '分析一下项目的架构', expectedType: IntentType.EXPLORATORY, expectedMode: IntentHandlerMode.EXPLORE },
  { prompt: '找到所有使用了useState的文件', expectedType: IntentType.EXPLORATORY, expectedMode: IntentHandlerMode.EXPLORE },
  
  // 明确任务类
  { prompt: '修改 src/index.ts 文件，添加日志功能', expectedType: IntentType.EXPLICIT, expectedMode: IntentHandlerMode.TASK },
  { prompt: '帮我实现一个登录页面', expectedType: IntentType.EXPLICIT, expectedMode: IntentHandlerMode.TASK },
  { prompt: '修复这个bug：用户无法登录', expectedType: IntentType.EXPLICIT, expectedMode: IntentHandlerMode.TASK },
  { prompt: '```typescript\nconst x = 1;\n```\n帮我优化这段代码', expectedType: IntentType.EXPLICIT, expectedMode: IntentHandlerMode.TASK },
  
  // 开放需求类
  { prompt: '改进系统的性能', expectedType: IntentType.OPEN_ENDED, expectedMode: IntentHandlerMode.TASK },
  { prompt: '优化用户体验', expectedType: IntentType.OPEN_ENDED, expectedMode: IntentHandlerMode.TASK },
];

const classifier = new IntentClassifier();
const gate = new IntentGate();

let passed = 0;
let failed = 0;

console.log('========== IntentClassifier 测试 ==========\n');

testCases.forEach((tc, index) => {
  const result = classifier.classify(tc.prompt);
  const typeMatch = result.type === tc.expectedType;
  const status = typeMatch ? '✅' : '❌';
  
  if (typeMatch) passed++;
  else failed++;
  
  console.log(`${status} [${index + 1}] "${tc.prompt.substring(0, 30)}${tc.prompt.length > 30 ? '...' : ''}"`);
  console.log(`   类型: ${result.type} (期望: ${tc.expectedType})`);
  console.log(`   置信度: ${(result.confidence * 100).toFixed(0)}%`);
  console.log(`   原因: ${result.reason}`);
  console.log();
});

console.log('========== IntentGate 测试 ==========\n');

testCases.forEach((tc, index) => {
  const result = gate.process(tc.prompt);
  const modeMatch = result.recommendedMode === tc.expectedMode;
  const status = modeMatch ? '✅' : '❌';
  
  console.log(`${status} [${index + 1}] "${tc.prompt.substring(0, 30)}${tc.prompt.length > 30 ? '...' : ''}"`);
  console.log(`   推荐模式: ${result.recommendedMode} (期望: ${tc.expectedMode})`);
  console.log(`   跳过任务分析: ${result.skipTaskAnalysis}`);
  console.log(`   需要澄清: ${result.needsClarification}`);
  console.log();
});

console.log('======================================================================');
console.log(`测试结果: ${passed} 通过, ${failed} 失败`);
console.log('======================================================================');

