"use strict";
/**
 * Claude 适配器测试脚本
 * 运行: npx ts-node test/test-claude-adapter.ts
 */
Object.defineProperty(exports, "__esModule", { value: true });
const claude_1 = require("../src/cli/adapters/claude");
async function testClaudeAdapter() {
    console.log('=== Claude 适配器测试 ===\n');
    const adapter = new claude_1.ClaudeAdapter({
        cwd: process.cwd(),
    });
    // 监听输出
    adapter.on('output', (chunk) => {
        console.log('[输出流]', chunk.substring(0, 100) + '...');
    });
    adapter.on('stateChange', (state) => {
        console.log('[状态变更]', state);
    });
    adapter.on('error', (error) => {
        console.error('[错误]', error.message);
    });
    try {
        // 连接
        console.log('1. 连接适配器...');
        await adapter.connect();
        console.log('   状态:', adapter.state, '已连接:', adapter.isConnected);
        // 发送消息
        console.log('\n2. 发送测试消息...');
        const response = await adapter.sendMessage('回复一个简单的 hi 即可');
        console.log('\n3. 响应结果:');
        console.log('   内容:', response.content);
        console.log('   完成:', response.done);
        console.log('   错误:', response.error || '无');
        console.log('   Session ID:', adapter.getSessionId());
        // 测试会话连续性
        console.log('\n4. 测试会话连续性...');
        const response2 = await adapter.sendMessage('你还记得我刚才说了什么吗？');
        console.log('   内容:', response2.content);
        // 断开连接
        console.log('\n5. 断开连接...');
        await adapter.disconnect();
        console.log('   状态:', adapter.state);
        console.log('\n=== 测试完成 ===');
    }
    catch (error) {
        console.error('测试失败:', error);
    }
}
testClaudeAdapter();
//# sourceMappingURL=test-claude-adapter.js.map