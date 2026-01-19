#!/usr/bin/env node

/**
 * 测试消息显示问题
 * 模拟前端接收消息的场景
 */

console.log('=== 测试消息显示问题 ===\n');

// 模拟前端状态
let currentSessionId = null; // 初始状态为 null
const activeSessionId = 'session-abc123'; // 后端发送的 sessionId

console.log('初始状态:');
console.log('  currentSessionId:', currentSessionId);
console.log('  activeSessionId (后端):', activeSessionId);
console.log();

// 测试场景
const testCases = [
  {
    name: '场景1: 初始状态收到消息',
    currentSessionId: null,
    msgSessionId: 'session-abc123',
    oldLogic: 'msg.sessionId && msg.sessionId !== currentSessionId',
    newLogic: 'currentSessionId && msg.sessionId && msg.sessionId !== currentSessionId'
  },
  {
    name: '场景2: Session ID 匹配',
    currentSessionId: 'session-abc123',
    msgSessionId: 'session-abc123',
    oldLogic: 'msg.sessionId && msg.sessionId !== currentSessionId',
    newLogic: 'currentSessionId && msg.sessionId && msg.sessionId !== currentSessionId'
  },
  {
    name: '场景3: Session ID 不匹配',
    currentSessionId: 'session-abc123',
    msgSessionId: 'session-xyz789',
    oldLogic: 'msg.sessionId && msg.sessionId !== currentSessionId',
    newLogic: 'currentSessionId && msg.sessionId && msg.sessionId !== currentSessionId'
  },
  {
    name: '场景4: 消息无 Session ID',
    currentSessionId: 'session-abc123',
    msgSessionId: null,
    oldLogic: 'msg.sessionId && msg.sessionId !== currentSessionId',
    newLogic: 'currentSessionId && msg.sessionId && msg.sessionId !== currentSessionId'
  }
];

testCases.forEach((testCase, index) => {
  console.log(`\n${testCase.name}`);
  console.log('─'.repeat(50));
  console.log(`  currentSessionId: ${testCase.currentSessionId}`);
  console.log(`  msg.sessionId: ${testCase.msgSessionId}`);

  // 旧逻辑
  const oldCondition = testCase.msgSessionId && testCase.msgSessionId !== testCase.currentSessionId;
  const oldResult = oldCondition ? '❌ 丢弃' : '✅ 接受';

  // 新逻辑
  const newCondition = testCase.currentSessionId && testCase.msgSessionId && testCase.msgSessionId !== testCase.currentSessionId;
  const newResult = newCondition ? '❌ 丢弃' : '✅ 接受';

  console.log(`  旧逻辑: ${oldResult}`);
  console.log(`    条件: ${testCase.oldLogic}`);
  console.log(`    结果: ${oldCondition}`);

  console.log(`  新逻辑: ${newResult}`);
  console.log(`    条件: ${testCase.newLogic}`);
  console.log(`    结果: ${newCondition}`);

  if (oldResult !== newResult) {
    console.log(`  ⚠️  行为变化: ${oldResult} → ${newResult}`);
  }
});

console.log('\n\n=== 问题总结 ===');
console.log('❌ 旧逻辑问题: 在初始状态(currentSessionId=null)时，');
console.log('   所有带 sessionId 的消息都会被丢弃');
console.log('✅ 新逻辑修复: 只有在 currentSessionId 已设置且不匹配时才丢弃');
console.log();
