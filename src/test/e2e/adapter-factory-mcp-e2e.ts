/**
 * 端对端测试：验证 LLMAdapterFactory 创建适配器时是否正确注入 MCP 和 Skills
 *
 * 🔧 测试重点：模拟插件实际运行时的适配器创建流程
 *
 * 运行方式：npm run compile && node out/test/e2e/adapter-factory-mcp-e2e.js
 */

// 在导入任何模块之前设置 vscode mock
const Module = require('module');
const originalLoad = Module._load;
Module._load = function(request: string, parent: any, isMain: boolean) {
  if (request === 'vscode') {
    return require('./vscode-mock');
  }
  return originalLoad.apply(this, [request, parent, isMain]);
};

import { LLMAdapterFactory } from '../../llm/adapter-factory';
import { MessageHub } from '../../orchestrator/core/message-hub';

async function main() {
  console.log('='.repeat(60));
  console.log('🧪 LLMAdapterFactory MCP/Skills 注入测试');
  console.log('='.repeat(60));

  const cwd = process.cwd();
  console.log(`\n📁 工作目录: ${cwd}\n`);

  // 1. 创建 AdapterFactory
  console.log('1️⃣ 创建 LLMAdapterFactory...');
  const factory = new LLMAdapterFactory({ cwd });
  console.log('   ✅ Factory 已创建');

  // 2. 创建 Mock MessageHub
  console.log('\n2️⃣ 创建 Mock MessageHub...');
  const messageHub = new MessageHub();
  factory.setMessageHub(messageHub);
  console.log('   ✅ MessageHub 已注入');

  // 3. 初始化（加载 Skills 和 MCP）
  console.log('\n3️⃣ 初始化 Factory（加载 Skills + MCP）...');
  await factory.initialize();
  console.log('   ✅ 初始化完成');

  // 4. 获取 Orchestrator 适配器并检查系统提示
  console.log('\n4️⃣ 获取 Orchestrator 适配器...');
  try {
    // 使用 sendMessage 触发适配器创建，然后获取
    // 先尝试通过接口获取（需要先发送消息才会创建适配器）
    // 直接调用私有方法 getOrCreateAdapter
    const orchestrator = (factory as any).getOrCreateAdapter('orchestrator');
    const systemPrompt = (orchestrator as any).getSystemPrompt?.() || '';
    
    console.log('   ✅ Orchestrator 适配器已创建');
    console.log(`   📏 系统提示长度: ${systemPrompt.length} 字符`);
    
    // 检查关键内容
    const checks = [
      { name: 'IDE 状态', pattern: /## IDE 状态/ },
      { name: '可用工具', pattern: /## 可用工具/ },
      { name: '内置工具', pattern: /### 内置工具/ },
      { name: 'MCP 工具', pattern: /### MCP 工具/ },
      { name: 'Skills/Prompts', pattern: /## 可用 Skills \/ Prompts/ },
      { name: '用户规则', pattern: /## 用户规则/ },
    ];
    
    console.log('\n   📋 系统提示内容检查:');
    for (const check of checks) {
      const found = check.pattern.test(systemPrompt);
      console.log(`      ${found ? '✅' : '❌'} ${check.name}`);
    }
    
    // 输出系统提示的前 2000 字符
    console.log('\n='.repeat(60));
    console.log('📜 Orchestrator 系统提示（前 3000 字符）:');
    console.log('='.repeat(60));
    console.log(systemPrompt.substring(0, 3000));
    if (systemPrompt.length > 3000) {
      console.log(`\n... [省略 ${systemPrompt.length - 3000} 字符]`);
    }
    console.log('='.repeat(60));
    
  } catch (error: any) {
    console.log(`   ❌ 获取 Orchestrator 失败: ${error.message}`);
  }

  // 5. 获取 Worker 适配器并检查系统提示
  console.log('\n5️⃣ 获取 Worker 适配器（claude）...');
  try {
    const worker = (factory as any).getOrCreateAdapter('claude');
    const systemPrompt = (worker as any).getSystemPrompt?.() || '';
    
    console.log('   ✅ Worker 适配器已创建');
    console.log(`   📏 系统提示长度: ${systemPrompt.length} 字符`);
    
    // 检查是否包含 MCP 工具信息
    const hasMCPTools = /### MCP 工具/.test(systemPrompt);
    const hasSkills = /## 可用 Skills/.test(systemPrompt);
    
    console.log(`   ${hasMCPTools ? '✅' : '❌'} Worker 能看到 MCP 工具`);
    console.log(`   ${hasSkills ? '✅' : '❌'} Worker 能看到 Skills`);
    
  } catch (error: any) {
    console.log(`   ❌ 获取 Worker 失败: ${error.message}`);
  }

  // 6. 检查 MCP 执行器
  console.log('\n6️⃣ 检查 MCP 执行器...');
  const mcpExecutor = factory.getMCPExecutor();
  if (mcpExecutor) {
    const tools = await mcpExecutor.getTools();
    const prompts = mcpExecutor.getPrompts();
    console.log(`   ✅ MCP 执行器存在`);
    console.log(`   📦 MCP 工具数量: ${tools.length}`);
    console.log(`   📝 MCP Prompts 数量: ${prompts.length}`);
    
    if (tools.length > 0) {
      console.log('   📦 前 5 个 MCP 工具:');
      tools.slice(0, 5).forEach(t => {
        console.log(`      - ${t.name}`);
      });
    }
  } else {
    console.log('   ❌ MCP 执行器不存在');
  }

  // 7. 清理
  console.log('\n7️⃣ 清理资源...');
  await factory.shutdown();
  console.log('   ✅ 资源已清理');

  console.log('\n✅ 测试完成！');
  process.exit(0);
}

main().catch(err => {
  console.error('❌ 测试失败:', err);
  process.exit(1);
});

