/**
 * 端对端测试：验证 EnvironmentContextProvider 能否正确获取所有工具和提示词
 *
 * 🔧 单一真相来源架构验证测试
 *
 * 运行方式：npm run compile && node out/test/e2e/environment-context-e2e.js
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

import { ToolManager } from '../../tools/tool-manager';
import { SkillsManager } from '../../tools/skills-manager';
import { MCPToolExecutor } from '../../tools/mcp-executor';
import { EnvironmentContextProvider } from '../../context/environment-context-provider';
import { LLMConfigLoader } from '../../llm/config';

async function main() {
  console.log('='.repeat(60));
  console.log('🧪 环境上下文端对端测试（单一真相来源架构验证）');
  console.log('='.repeat(60));

  const cwd = process.cwd();
  console.log(`\n📁 工作目录: ${cwd}\n`);

  // 1. 创建 ToolManager
  console.log('1️⃣ 创建 ToolManager...');
  const toolManager = new ToolManager(cwd);
  console.log('   ✅ ToolManager 已创建');

  // 2. 加载并注册 Skills
  console.log('\n2️⃣ 加载并注册 Skills...');
  try {
    const skillsConfig = LLMConfigLoader.loadSkillsConfig();
    const skillsManager = new SkillsManager({
      customTools: skillsConfig?.customTools || [],
      instructionSkills: skillsConfig?.instructionSkills || [],
    });
    toolManager.registerSkillExecutor(skillsManager);

    const instructionSkills = skillsManager.getInstructionSkills();
    console.log(`   ✅ Skills 已加载: ${instructionSkills.length} 个 instructionSkills`);
    instructionSkills.forEach(s => console.log(`      - ${s.name}: ${s.description || '(无描述)'}`));
  } catch (error: any) {
    console.log(`   ⚠️ Skills 加载失败: ${error.message}`);
  }

  // 3. 加载并注册 MCP
  console.log('\n3️⃣ 加载并注册 MCP...');
  let mcpExecutor: MCPToolExecutor | null = null;
  try {
    mcpExecutor = new MCPToolExecutor();
    await mcpExecutor.initialize();
    toolManager.registerMCPExecutor('mcp-servers', mcpExecutor);

    const mcpTools = await mcpExecutor.getTools();
    const mcpPrompts = mcpExecutor.getPrompts();
    console.log(`   ✅ MCP 已加载: ${mcpTools.length} 个工具, ${mcpPrompts.length} 个 Prompts`);
    if (mcpTools.length > 0) {
      console.log('   📦 MCP 工具:');
      mcpTools.slice(0, 5).forEach(t => console.log(`      - ${t.name}: ${t.description || '(无描述)'}`));
      if (mcpTools.length > 5) console.log(`      ... 还有 ${mcpTools.length - 5} 个`);
    }
    if (mcpPrompts.length > 0) {
      console.log('   📝 MCP Prompts:');
      mcpPrompts.forEach(p => console.log(`      - ${p.name}: ${p.description || '(无描述)'}`));
    }
  } catch (error: any) {
    console.log(`   ⚠️ MCP 加载失败: ${error.message}`);
  }

  // 4. 创建 EnvironmentContextProvider
  console.log('\n4️⃣ 创建 EnvironmentContextProvider...');
  const provider = new EnvironmentContextProvider({ workspace: cwd });
  provider.setToolManager(toolManager);
  console.log('   ✅ EnvironmentContextProvider 已创建并注入 ToolManager');

  // 5. 刷新缓存
  console.log('\n5️⃣ 刷新环境上下文缓存...');
  await provider.refresh();
  console.log('   ✅ 缓存已刷新');

  // 6. 获取并输出环境提示
  console.log('\n6️⃣ 获取环境提示...');
  const envPrompt = provider.getEnvironmentPrompt();
  console.log('='.repeat(60));
  console.log('📜 生成的环境提示内容:');
  console.log('='.repeat(60));
  console.log(envPrompt);
  console.log('='.repeat(60));

  // 7. 验证 ToolManager.getPrompts()
  console.log('\n7️⃣ 验证 ToolManager.getPrompts()...');
  const allPrompts = toolManager.getPrompts();
  console.log(`   📊 总计 ${allPrompts.length} 个 Prompts`);
  console.log(`      - MCP Prompts: ${allPrompts.filter(p => p.source === 'mcp').length}`);
  console.log(`      - Instruction Skills: ${allPrompts.filter(p => p.source === 'skill').length}`);

  // 8. 验证 ToolManager.getTools()
  console.log('\n8️⃣ 验证 ToolManager.getTools()...');
  const allTools = await toolManager.getTools();
  console.log(`   📊 总计 ${allTools.length} 个工具`);
  console.log(`      - Builtin: ${allTools.filter(t => t.metadata?.source === 'builtin').length}`);
  console.log(`      - MCP: ${allTools.filter(t => t.metadata?.source === 'mcp').length}`);
  console.log(`      - Skill: ${allTools.filter(t => t.metadata?.source === 'skill').length}`);

  // 9. 清理
  console.log('\n9️⃣ 清理资源...');
  if (mcpExecutor) {
    await mcpExecutor.shutdown();
  }
  console.log('   ✅ 资源已清理');

  console.log('\n✅ 测试完成！');
  process.exit(0);
}

main().catch(err => {
  console.error('❌ 测试失败:', err);
  process.exit(1);
});

