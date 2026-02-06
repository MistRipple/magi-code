/**
 * 针对性测试：验证 delegationBriefing 自然语言任务派发
 *
 * 测试点：
 * 1. Prompt 模板是否包含 delegationBriefings 要求
 * 2. Orchestrator LLM 是否返回 delegationBriefings 字段
 */

import { LLMAdapterFactory } from '../../llm/adapter-factory';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { buildRequirementAnalysisPrompt } from '../../orchestrator/prompts/orchestrator-prompts';
import { ProfileLoader } from '../../orchestrator/profile/profile-loader';

async function testDelegationBriefing() {
  console.log('============================================================');
  console.log('测试：delegationBriefing 自然语言任务派发');
  console.log('============================================================\n');

  const workspaceRoot = process.cwd();

  // 1. 测试 Prompt 格式
  console.log('【测试 1】检查 Prompt 模板是否包含 delegationBriefings 要求');
  console.log('------------------------------------------------------------');

  const profileLoader = ProfileLoader.getInstance();
  const categoryHints = Array.from(profileLoader.getAllCategories().entries())
    .map((entry: [string, any]) => `- ${entry[0]}: ${entry[1].description}`)
    .slice(0, 3)
    .join('\n');
  const prompt = buildRequirementAnalysisPrompt(
    '分析 src/types.ts 文件的类型定义',
    'TASK',
    categoryHints
  );

  const hasDelegationBriefingsInPrompt = prompt.includes('delegationBriefings');
  console.log(`Prompt 包含 delegationBriefings 要求: ${hasDelegationBriefingsInPrompt ? '✓ 通过' : '✗ 失败'}`);

  if (!hasDelegationBriefingsInPrompt) {
    console.log('\nPrompt 内容片段:');
    const startIndex = prompt.indexOf('## 输出格式');
    if (startIndex >= 0) {
      console.log(prompt.slice(startIndex, startIndex + 500));
    }
  }

  // 2. 测试 LLM 响应
  console.log('\n【测试 2】测试 LLM 是否返回 delegationBriefings');
  console.log('------------------------------------------------------------');

  const adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });
  const messageHub = new MessageHub();
  adapterFactory.setMessageHub(messageHub);
  await adapterFactory.initialize();

  const testPrompt = buildRequirementAnalysisPrompt(
    '分析 src/types.ts 文件中的类型定义，给出改进建议',
    'TASK',
    categoryHints
  );

  console.log('发送请求到 Orchestrator LLM...');

  const response = await adapterFactory.sendMessage(
    'orchestrator',
    testPrompt,
    undefined,
    {
      source: 'orchestrator',
      visibility: 'system',  // 🔧 v2: 使用 visibility
      adapterRole: 'orchestrator',
    }
  );

  console.log('\nLLM 响应状态:', response.error ? '失败' : '成功');

  if (response.content) {
    const jsonMatch = response.content.match(/\{[\s\S]*\}/);
    if (jsonMatch) {
      try {
        const parsed = JSON.parse(jsonMatch[0]);
        console.log('\n解析的 JSON 结构:');
        console.log('- needsWorker:', parsed.needsWorker);
        console.log('- delegationBriefing:', parsed.delegationBriefing ? '存在' : '不存在');
        console.log('- reason:', parsed.reason);

        if (parsed.delegationBriefing && typeof parsed.delegationBriefing === 'string') {
          console.log('\n✓ delegationBriefing 字段存在!');
          console.log('\n委托说明内容（自然语言）:');
          console.log(`  ${parsed.delegationBriefing.slice(0, 300)}${parsed.delegationBriefing.length > 300 ? '...' : ''}`);
        } else {
          console.log('\n✗ delegationBriefing 字段不存在或格式错误');
          console.log('完整 JSON:', JSON.stringify(parsed, null, 2));
        }
      } catch (e) {
        console.log('JSON 解析失败:', e);
        console.log('原始内容:', response.content.slice(0, 500));
      }
    } else {
      console.log('未找到 JSON 结构');
      console.log('原始内容:', response.content.slice(0, 500));
    }
  }

  // 清理
  await adapterFactory.shutdown();

  console.log('\n============================================================');
  console.log('测试完成');
  console.log('============================================================');
}

// 运行测试
testDelegationBriefing().catch(console.error);
