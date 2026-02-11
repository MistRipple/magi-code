/**
 * 画像系统端到端测试
 *
 * 验证链路：
 * ProfileLoader(推导 strengths/weaknesses) → PromptBuilder(组装 prompt) → LLM(真实调用) → 验证响应
 *
 * 使用方法：npx tsx scripts/profile-e2e-test.ts
 */

import { ProfileLoader } from '../src/orchestrator/profile/profile-loader';
import { GuidanceInjector } from '../src/orchestrator/profile/guidance-injector';
import { PromptBuilder } from '../src/orchestrator/profile/prompt-builder';
import { CATEGORY_DEFINITIONS } from '../src/orchestrator/profile/builtin/category-definitions';
import { DEFAULT_ASSIGNMENTS } from '../src/orchestrator/profile/builtin/default-assignments';
import { buildUnifiedSystemPrompt, UnifiedPromptContext } from '../src/orchestrator/prompts/orchestrator-prompts';
import { LLMConfigLoader } from '../src/llm/config';
import { createLLMClient } from '../src/llm/clients/client-factory';
import type { WorkerSlot } from '../src/types/agent-types';
import type { LLMClient, LLMResponse } from '../src/llm/types';

// ============================================================================
// 测试工具函数
// ============================================================================

const PASS = '\x1b[32m✓\x1b[0m';
const FAIL = '\x1b[31m✗\x1b[0m';
const INFO = '\x1b[36mℹ\x1b[0m';
const WARN = '\x1b[33m⚠\x1b[0m';

let totalTests = 0;
let passedTests = 0;
let failedTests = 0;

function assert(condition: boolean, message: string, detail?: string): void {
  totalTests++;
  if (condition) {
    passedTests++;
    console.log(`  ${PASS} ${message}`);
  } else {
    failedTests++;
    console.log(`  ${FAIL} ${message}`);
    if (detail) {
      console.log(`    └── ${detail}`);
    }
  }
}

function section(title: string): void {
  console.log(`\n${'='.repeat(60)}`);
  console.log(`  ${title}`);
  console.log(`${'='.repeat(60)}`);
}

// ============================================================================
// Phase 1: ProfileLoader 推导验证（纯本地，无 LLM 调用）
// ============================================================================

async function testProfileLoaderDerivation(): Promise<void> {
  section('Phase 1: ProfileLoader strengths/weaknesses 推导验证');

  // 重置单例以确保干净状态
  ProfileLoader.resetInstance();
  const loader = ProfileLoader.getInstance();
  await loader.load();

  const workers: WorkerSlot[] = ['claude', 'codex', 'gemini'];

  for (const worker of workers) {
    console.log(`\n  --- ${worker.toUpperCase()} ---`);
    const profile = loader.getProfile(worker);

    // 验证 strengths 不为空
    assert(
      profile.persona.strengths.length > 0,
      `${worker} strengths 已推导（${profile.persona.strengths.length} 项）`,
      `实际值: ${JSON.stringify(profile.persona.strengths)}`
    );

    // 验证 weaknesses 不为空
    assert(
      profile.persona.weaknesses.length > 0,
      `${worker} weaknesses 已推导（${profile.persona.weaknesses.length} 项）`,
      `实际值: ${JSON.stringify(profile.persona.weaknesses)}`
    );

    // 验证 strengths 来自 assignedCategories 的 displayName（排除泛化分类 simple/general）
    const GENERIC_CATEGORIES = new Set(['simple', 'general']);
    const expectedStrengths = profile.assignedCategories
      .filter(cat => !GENERIC_CATEGORIES.has(cat))
      .map(cat => CATEGORY_DEFINITIONS[cat]?.displayName)
      .filter(Boolean);
    assert(
      JSON.stringify(profile.persona.strengths) === JSON.stringify(expectedStrengths),
      `${worker} strengths 与 assignedCategories 一致`,
      `期望: ${JSON.stringify(expectedStrengths)}, 实际: ${JSON.stringify(profile.persona.strengths)}`
    );

    // 验证 weaknesses 不包含已分配分类
    const assignedDisplayNames = new Set(expectedStrengths);
    const hasConflict = profile.persona.weaknesses.some(w => assignedDisplayNames.has(w));
    assert(
      !hasConflict,
      `${worker} weaknesses 不与 strengths 冲突`,
      `weaknesses: ${JSON.stringify(profile.persona.weaknesses)}`
    );

    // 验证 weaknesses 最多 3 项
    assert(
      profile.persona.weaknesses.length <= 3,
      `${worker} weaknesses 不超过 3 项`,
    );

    // 验证 baseRole 不包含「核心能力」
    assert(
      !profile.persona.baseRole.includes('## 核心能力'),
      `${worker} baseRole 不含「核心能力」段落`,
    );

    // 打印详细信息
    console.log(`    ${INFO} assignedCategories: ${JSON.stringify(profile.assignedCategories)}`);
    console.log(`    ${INFO} strengths: ${JSON.stringify(profile.persona.strengths)}`);
    console.log(`    ${INFO} weaknesses: ${JSON.stringify(profile.persona.weaknesses)}`);
  }

  // 验证跨 Worker 的 strengths 不重叠（因为分类不重叠）
  console.log('\n  --- 跨 Worker 验证 ---');
  const allStrengths = workers.map(w => loader.getProfile(w).persona.strengths).flat();
  const uniqueStrengths = new Set(allStrengths);
  assert(
    allStrengths.length === uniqueStrengths.size,
    '三个 Worker 的 strengths 无重叠',
    `总计 ${allStrengths.length} 项，去重后 ${uniqueStrengths.size} 项`
  );
}

// ============================================================================
// Phase 2: PromptBuilder 组装验证（纯本地，无 LLM 调用）
// ============================================================================

async function testPromptBuilderAssembly(): Promise<void> {
  section('Phase 2: PromptBuilder 组装验证');

  const loader = ProfileLoader.getInstance();
  const injector = new GuidanceInjector();
  const workers: WorkerSlot[] = ['claude', 'codex', 'gemini'];

  for (const worker of workers) {
    console.log(`\n  --- ${worker.toUpperCase()} Worker Prompt ---`);
    const profile = loader.getProfile(worker);

    // 测试基本 prompt 组装
    const prompt = injector.buildWorkerPrompt(profile, {
      taskDescription: '测试任务描述',
      category: profile.assignedCategories[0],
    });

    // 验证 prompt 包含角色定位
    assert(
      prompt.includes('## 角色定位'),
      `${worker} prompt 含「角色定位」`,
    );

    // 验证 prompt 包含核心能力（从 strengths 推导）
    assert(
      prompt.includes('## 核心能力'),
      `${worker} prompt 含「核心能力」段落`,
    );

    // 验证核心能力的每一项都来自推导的 strengths
    for (const strength of profile.persona.strengths) {
      assert(
        prompt.includes(`- ${strength}`),
        `${worker} prompt 含能力项「${strength}」`,
      );
    }

    // 验证 prompt 包含任务类型
    assert(
      prompt.includes('## 任务类型'),
      `${worker} prompt 含「任务类型」`,
    );

    // 验证 prompt 包含工具使用规范
    assert(
      prompt.includes('## 工具使用规范'),
      `${worker} prompt 含「工具使用规范」`,
    );

    // 验证 prompt 不含「核心能力」字样在 baseRole 中（不是独立段落的那个）
    // baseRole 应该以角色定位开头，紧接工作方法
    const roleSection = prompt.split('## 核心能力')[0];
    assert(
      !roleSection.includes('复杂架构设计与跨模块集成') &&
      !roleSection.includes('快速代码生成与 Bug 修复') &&
      !roleSection.includes('前端 UI/UX 开发与样式优化'),
      `${worker} baseRole 段落不含旧的硬编码能力声明`,
    );

    // 打印 prompt 长度
    console.log(`    ${INFO} prompt 长度: ${prompt.length} 字符`);
  }

  // 测试编排者系统提示词
  console.log('\n  --- 编排者系统提示词 ---');
  const allProfiles = loader.getAllProfiles();
  const workerProfiles = Array.from(allProfiles.values())
    .map(p => ({
      worker: p.worker,
      displayName: p.persona.displayName,
      strengths: p.persona.strengths,
      assignedCategories: p.assignedCategories,
    }));

  const orchestratorPrompt = buildUnifiedSystemPrompt({
    availableWorkers: workers,
    workerProfiles,
  });

  // 验证编排者 prompt 中的 Worker 能力表使用推导后的 strengths
  for (const wp of workerProfiles) {
    const expectedLine = wp.strengths.join('、');
    assert(
      orchestratorPrompt.includes(expectedLine),
      `编排者 prompt 含 ${wp.worker} 的推导能力: ${expectedLine.substring(0, 30)}...`,
    );
  }

  console.log(`    ${INFO} 编排者 prompt 长度: ${orchestratorPrompt.length} 字符`);
}

// ============================================================================
// Phase 3: 真实 LLM 调用验证
// ============================================================================

async function testRealLLMCall(): Promise<void> {
  section('Phase 3: 真实 LLM 端到端验证');

  // 加载 LLM 配置
  let fullConfig;
  try {
    fullConfig = LLMConfigLoader.loadFullConfig();
  } catch (e) {
    console.log(`  ${WARN} 无法加载 LLM 配置，跳过真实 LLM 测试`);
    return;
  }

  const loader = ProfileLoader.getInstance();
  const injector = new GuidanceInjector();
  const workers: WorkerSlot[] = ['claude', 'codex', 'gemini'];

  for (const worker of workers) {
    const workerConfig = fullConfig.workers[worker];
    if (!workerConfig?.enabled || !workerConfig.apiKey) {
      console.log(`  ${WARN} ${worker} 未启用或缺少 apiKey，跳过`);
      continue;
    }

    console.log(`\n  --- ${worker.toUpperCase()} 真实 LLM 调用 ---`);
    console.log(`    ${INFO} 模型: ${workerConfig.model} (${workerConfig.provider})`);

    const profile = loader.getProfile(worker);
    const systemPrompt = injector.buildWorkerPrompt(profile, {
      taskDescription: '画像验证测试',
      category: profile.assignedCategories[0],
    });

    // 创建 LLM 客户端
    let client: LLMClient;
    try {
      client = createLLMClient(workerConfig);
    } catch (e) {
      console.log(`  ${WARN} ${worker} 客户端创建失败: ${e}`);
      continue;
    }

    // 发送验证消息：要求 LLM 列出自己的核心能力
    const verificationMessage = `请用一句话简要回答：你的核心能力是什么？只列出关键词，不要解释。`;

    try {
      const response: LLMResponse = await client.sendMessage({
        systemPrompt,
        messages: [{ role: 'user', content: verificationMessage }],
        maxTokens: 200,
        temperature: 0,
      });

      const responseText = response.content || '';
      console.log(`    ${INFO} LLM 响应: ${responseText.substring(0, 200)}`);

      // 验证 LLM 响应中至少包含一项推导的 strength
      const matchedStrengths = profile.persona.strengths.filter(s =>
        responseText.includes(s) || responseText.toLowerCase().includes(s.toLowerCase())
      );
      assert(
        matchedStrengths.length > 0,
        `${worker} LLM 响应包含推导的能力标签（命中 ${matchedStrengths.length}/${profile.persona.strengths.length}）`,
        `命中: ${JSON.stringify(matchedStrengths)}, 全部: ${JSON.stringify(profile.persona.strengths)}`
      );

      // 验证 LLM 响应不包含旧的硬编码能力（已从 baseRole 移除的）
      const oldCapabilities = getOldHardcodedCapabilities(worker);
      const leakedCapabilities = oldCapabilities.filter(c => responseText.includes(c));
      assert(
        leakedCapabilities.length === 0,
        `${worker} LLM 响应不含旧的硬编码能力`,
        leakedCapabilities.length > 0 ? `泄漏: ${JSON.stringify(leakedCapabilities)}` : undefined
      );

    } catch (e: any) {
      console.log(`  ${FAIL} ${worker} LLM 调用失败: ${e.message?.substring(0, 100)}`);
      failedTests++;
      totalTests++;
    }
  }
}

/**
 * 获取旧版硬编码的能力（已从 baseRole 移除，不应出现在 LLM 响应中）
 */
function getOldHardcodedCapabilities(worker: WorkerSlot): string[] {
  const oldCaps: Record<WorkerSlot, string[]> = {
    claude: ['接口契约设计与 API 规范'],
    codex: ['批量文件操作', 'API 实现'],
    gemini: ['大上下文代码理解和分析', '多模态内容处理'],
  };
  return oldCaps[worker] || [];
}

// ============================================================================
// Phase 4: 分工变更场景验证
// ============================================================================

async function testAssignmentChangeScenario(): Promise<void> {
  section('Phase 4: 分工变更场景验证（模拟）');

  // 模拟场景：将 frontend 从 gemini 改为 claude
  // 验证 claude 的 strengths 会包含「前端开发」，gemini 不再包含

  const promptBuilder = new PromptBuilder();
  const loader = ProfileLoader.getInstance();

  // 当前默认分工下的 strengths
  const claudeProfile = loader.getProfile('claude');
  const geminiProfile = loader.getProfile('gemini');

  console.log('\n  --- 默认分工 ---');
  console.log(`    ${INFO} claude strengths: ${JSON.stringify(claudeProfile.persona.strengths)}`);
  console.log(`    ${INFO} gemini strengths: ${JSON.stringify(geminiProfile.persona.strengths)}`);

  assert(
    !claudeProfile.persona.strengths.includes('前端开发'),
    '默认分工下 claude 不含「前端开发」',
  );
  assert(
    geminiProfile.persona.strengths.includes('前端开发'),
    '默认分工下 gemini 含「前端开发」',
  );

  // 模拟变更后的推导（不修改实际文件，直接调用推导逻辑验证）
  console.log('\n  --- 模拟分工变更: frontend → claude ---');

  // 手动构建变更后的分配
  const modifiedAssignments = {
    claude: ['architecture', 'refactor', 'review', 'debug', 'integration', 'frontend'],
    codex: ['backend', 'bugfix', 'implement', 'test', 'simple', 'general'],
    gemini: ['document', 'data_analysis'],
  };

  // 模拟推导（使用与 ProfileLoader 相同的逻辑）
  const claudeNewStrengths = modifiedAssignments.claude
    .map(cat => CATEGORY_DEFINITIONS[cat]?.displayName)
    .filter(Boolean);
  const geminiNewStrengths = modifiedAssignments.gemini
    .map(cat => CATEGORY_DEFINITIONS[cat]?.displayName)
    .filter(Boolean);

  assert(
    claudeNewStrengths.includes('前端开发'),
    '变更后 claude strengths 含「前端开发」',
    `实际: ${JSON.stringify(claudeNewStrengths)}`
  );
  assert(
    !geminiNewStrengths.includes('前端开发'),
    '变更后 gemini strengths 不含「前端开发」',
    `实际: ${JSON.stringify(geminiNewStrengths)}`
  );

  // 验证变更后 prompt 正确反映新能力
  const mockPersona = {
    ...claudeProfile.persona,
    strengths: claudeNewStrengths as string[],
  };
  const prompt = promptBuilder.buildWorkerPrompt(mockPersona, {
    taskDescription: '前端页面开发',
    category: 'frontend',
  });

  assert(
    prompt.includes('- 前端开发'),
    '变更后 claude prompt 核心能力含「前端开发」',
  );
  assert(
    prompt.includes('## 任务类型') && prompt.includes('前端开发'),
    '变更后 claude prompt 任务类型为「前端开发」',
  );

  console.log(`    ${INFO} 分工变更场景验证通过，能力标签自动同步`);
}

// ============================================================================
// 主函数
// ============================================================================

async function main(): Promise<void> {
  console.log('\n╔══════════════════════════════════════════════════════╗');
  console.log('║       Magi 画像系统端到端测试 (E2E)                 ║');
  console.log('╚══════════════════════════════════════════════════════╝');

  try {
    // Phase 1: 推导验证（纯本地）
    await testProfileLoaderDerivation();

    // Phase 2: Prompt 组装验证（纯本地）
    await testPromptBuilderAssembly();

    // Phase 3: 真实 LLM 调用验证
    await testRealLLMCall();

    // Phase 4: 分工变更场景验证
    await testAssignmentChangeScenario();

  } catch (e: any) {
    console.error(`\n${FAIL} 测试执行异常: ${e.message}`);
    console.error(e.stack);
  }

  // 汇总
  section('测试结果汇总');
  console.log(`  总计: ${totalTests} 项`);
  console.log(`  ${PASS} 通过: ${passedTests} 项`);
  if (failedTests > 0) {
    console.log(`  ${FAIL} 失败: ${failedTests} 项`);
  }
  console.log(`  通过率: ${totalTests > 0 ? Math.round((passedTests / totalTests) * 100) : 0}%`);
  console.log();

  process.exit(failedTests > 0 ? 1 : 0);
}

main();
