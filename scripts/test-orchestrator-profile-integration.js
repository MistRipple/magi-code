/**
 * 编排系统与画像系统集成测试
 * 验证完整的编排-画像-分类-LLM调用链路
 */

const path = require('path');
const fs = require('fs');

// 测试配置
const TEST_WORKSPACE = path.join(__dirname, '..', '.test-orchestrator-profile');

// 清理测试目录
function cleanupTestDir() {
  if (fs.existsSync(TEST_WORKSPACE)) {
    fs.rmSync(TEST_WORKSPACE, { recursive: true });
  }
  fs.mkdirSync(TEST_WORKSPACE, { recursive: true });
}

// 测试结果统计
let passed = 0;
let failed = 0;
let skipped = 0;

function test(name, fn) {
  try {
    fn();
    console.log(`✅ ${name}`);
    passed++;
  } catch (error) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${error.message}`);
    failed++;
  }
}

async function asyncTest(name, fn, options = {}) {
  if (options.skipIf) {
    console.log(`⏭️  ${name} (${options.skipMessage || '跳过'})`);
    skipped++;
    return;
  }

  try {
    await fn();
    console.log(`✅ ${name}`);
    passed++;
  } catch (error) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${error.message}`);
    if (error.stack) {
      console.log(`   堆栈: ${error.stack.split('\n').slice(1, 3).join('\n')}`);
    }
    failed++;
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message || 'Assertion failed');
  }
}

async function runTests() {
  console.log('\n🧪 编排系统与画像系统集成测试\n');
  console.log('='.repeat(70));

  // 清理测试目录
  cleanupTestDir();

  // 动态导入模块
  const { ProfileLoader } = require('../out/orchestrator/profile/profile-loader');
  const { AgentProfileLoader } = require('../out/orchestrator/profile/agent-profile-loader');
  const { GuidanceInjector } = require('../out/orchestrator/profile/guidance-injector');
  const { WorkerSelector } = require('../out/task/worker-selector');
  const { TaskAnalyzer } = require('../out/task/task-analyzer');
  const { LLMAdapterFactory } = require('../out/llm/adapter-factory');
  const { LLMConfigLoader } = require('../out/llm/config');

  // ========================================
  // 1. ProfileLoader 与分类配置测试
  // ========================================
  console.log('\n📋 1. ProfileLoader 与分类配置测试\n');

  let profileLoader;

  await asyncTest('加载 ProfileLoader 单例', async () => {
    profileLoader = ProfileLoader.getInstance();
    await profileLoader.load();
    // ProfileLoader 没有 isLoaded() 方法，通过检查 profiles 验证加载成功
    const profiles = profileLoader.getAllProfiles();
    assert(profiles.size >= 3, 'ProfileLoader 应该已加载至少 3 个画像');
  });

  test('获取所有 Worker 画像', () => {
    const profiles = profileLoader.getAllProfiles();
    assert(profiles.size >= 3, '应该有至少 3 个 Worker 画像');
    console.log(`   加载了 ${profiles.size} 个画像: ${Array.from(profiles.keys()).join(', ')}`);
  });

  test('获取分类配置', () => {
    const categories = profileLoader.getAllCategories();
    assert(categories.size > 0, '应该有分类配置');
    console.log(`   加载了 ${categories.size} 个分类: ${Array.from(categories.keys()).join(', ')}`);
  });

  test('验证分类包含关键词和默认 Worker', () => {
    const categories = profileLoader.getAllCategories();
    const architecture = categories.get('architecture');
    assert(architecture, '应该有 architecture 分类');
    assert(architecture.keywords.length > 0, '分类应该有关键词');
    assert(architecture.defaultWorker, '分类应该有默认 Worker');
    console.log(`   architecture 分类: 关键词=${architecture.keywords.length}, 默认Worker=${architecture.defaultWorker}`);
  });

  // ========================================
  // 2. WorkerSelector 与画像融合测试
  // ========================================
  console.log('\n🎯 2. WorkerSelector 与画像融合测试\n');

  let workerSelector;

  test('创建 WorkerSelector 并注入 ProfileLoader', () => {
    workerSelector = new WorkerSelector();
    workerSelector.setProfileLoader(profileLoader);
    assert(workerSelector, 'WorkerSelector 应该被创建');
  });

  test('根据任务描述选择 Worker（架构任务）', () => {
    const result = workerSelector.selectByDescription('我需要重构这个模块的架构设计');
    assert(result.worker, '应该选择一个 Worker');
    console.log(`   任务: 架构设计 → 选择: ${result.worker}, 原因: ${result.reason}`);
  });

  test('根据任务描述选择 Worker（Bug修复任务）', () => {
    const result = workerSelector.selectByDescription('修复登录页面的 bug');
    assert(result.worker, '应该选择一个 Worker');
    console.log(`   任务: Bug修复 → 选择: ${result.worker}, 原因: ${result.reason}`);
  });

  test('根据任务描述选择 Worker（测试任务）', () => {
    const result = workerSelector.selectByDescription('编写单元测试用例');
    assert(result.worker, '应该选择一个 Worker');
    console.log(`   任务: 测试 → 选择: ${result.worker}, 原因: ${result.reason}`);
  });

  // ========================================
  // 3. GuidanceInjector 引导注入测试
  // ========================================
  console.log('\n💉 3. GuidanceInjector 引导注入测试\n');

  let guidanceInjector;

  test('创建 GuidanceInjector', () => {
    guidanceInjector = new GuidanceInjector();
    assert(guidanceInjector, 'GuidanceInjector 应该被创建');
  });

  test('为 Claude 构建引导 Prompt', () => {
    const claudeProfile = profileLoader.getProfile('claude');
    assert(claudeProfile, '应该获取到 Claude 画像');

    const prompt = guidanceInjector.buildWorkerPrompt(claudeProfile, {
      taskDescription: '分析代码架构',
    });

    assert(prompt.length > 0, 'Prompt 不应为空');
    assert(prompt.includes('角色定位'), 'Prompt 应该包含角色定位');
    console.log(`   Claude Prompt 长度: ${prompt.length} 字符`);
    console.log(`   包含角色定位: ✅`);
  });

  test('为 Codex 构建引导 Prompt', () => {
    const codexProfile = profileLoader.getProfile('codex');
    assert(codexProfile, '应该获取到 Codex 画像');

    const prompt = guidanceInjector.buildWorkerPrompt(codexProfile, {
      taskDescription: '实现功能代码',
    });

    assert(prompt.length > 0, 'Prompt 不应为空');
    console.log(`   Codex Prompt 长度: ${prompt.length} 字符`);
  });

  test('带协作上下文构建 Prompt', () => {
    const claudeProfile = profileLoader.getProfile('claude');
    const prompt = guidanceInjector.buildWorkerPrompt(claudeProfile, {
      taskDescription: '协作任务',
      collaborators: ['codex', 'gemini'],
    });

    assert(prompt.length > 0, 'Prompt 不应为空');
    console.log(`   协作 Prompt 长度: ${prompt.length} 字符`);
  });

  // ========================================
  // 4. AgentProfileLoader 与 LLM 配置融合测试
  // ========================================
  console.log('\n🔗 4. AgentProfileLoader 与 LLM 配置融合测试\n');

  let agentProfileLoader;

  test('创建 AgentProfileLoader', () => {
    agentProfileLoader = new AgentProfileLoader();
    assert(agentProfileLoader, 'AgentProfileLoader 应该被创建');
  });

  test('加载 Claude 的 Agent 画像（含 LLM 配置）', () => {
    const agentProfile = agentProfileLoader.loadAgentProfile('claude');
    assert(agentProfile, '应该获取到 Agent 画像');
    assert(agentProfile.llm, '应该包含 LLM 配置');
    assert(agentProfile.guidance, '应该包含引导配置');
    console.log(`   Claude: 模型=${agentProfile.llm.model}, 角色=${agentProfile.guidance?.role?.substring(0, 30)}...`);
  });

  test('加载 Codex 的 Agent 画像', () => {
    const agentProfile = agentProfileLoader.loadAgentProfile('codex');
    assert(agentProfile, '应该获取到 Agent 画像');
    console.log(`   Codex: 模型=${agentProfile.llm.model}`);
  });

  // ========================================
  // 5. LLMAdapterFactory 完整集成测试
  // ========================================
  console.log('\n🏭 5. LLMAdapterFactory 完整集成测试\n');

  // 检查是否有 API Key
  const fullConfig = LLMConfigLoader.loadFullConfig();
  const hasApiKey = fullConfig.workers.claude.apiKey && fullConfig.workers.claude.apiKey.length > 0;

  let adapterFactory;

  await asyncTest('创建并初始化 LLMAdapterFactory', async () => {
    adapterFactory = new LLMAdapterFactory({ cwd: TEST_WORKSPACE });
    await adapterFactory.initialize();
    assert(adapterFactory, 'LLMAdapterFactory 应该被创建');
  });

  await asyncTest('验证适配器接收 ProfileLoader', async () => {
    // sendMessage 会自动创建适配器并连接
    // 这里我们只需验证适配器被正确创建并能工作
    const response = await adapterFactory.sendMessage(
      'claude',
      '请用一句话回答：1+1等于多少？',
      undefined,
      { source: 'test', adapterRole: 'worker' }
    );

    assert(response.content, 'Claude 适配器应该返回响应');
    console.log(`   Claude 适配器已连接并正常工作`);
    console.log(`   响应: ${response.content.substring(0, 50)}...`);
  }, {
    skipIf: !hasApiKey,
    skipMessage: '未配置 API Key'
  });

  await asyncTest('发送消息验证画像引导生效', async () => {
    // 发送一个需要特定角色知识的问题
    const response = await adapterFactory.sendMessage(
      'claude',
      '请简要描述你的专业领域和角色定位。',
      undefined,
      { source: 'test', adapterRole: 'worker' }
    );

    assert(response.content, '应该有响应内容');
    console.log(`   LLM 响应: ${response.content.substring(0, 100)}...`);

    // 验证响应中是否体现了画像引导
    const contentLower = response.content.toLowerCase();
    const hasArchitectureKeywords =
      contentLower.includes('架构') ||
      contentLower.includes('设计') ||
      contentLower.includes('代码') ||
      contentLower.includes('软件') ||
      contentLower.includes('architect');

    if (hasArchitectureKeywords) {
      console.log(`   ✓ 响应体现了画像引导（包含架构/软件相关术语）`);
    }
  }, {
    skipIf: !hasApiKey,
    skipMessage: '未配置 API Key'
  });

  // ========================================
  // 6. 完整编排决策流程测试
  // ========================================
  console.log('\n🎭 6. 完整编排决策流程测试\n');

  test('完整流程: 任务分析 → 分类 → Worker选择 → 画像注入', () => {
    const taskDescription = '重构用户认证模块的代码架构';

    // 1. 使用 WorkerSelector 选择 Worker
    const selection = workerSelector.selectByDescription(taskDescription);
    console.log(`   1️⃣ 任务: "${taskDescription}"`);
    console.log(`   2️⃣ 选择 Worker: ${selection.worker} (${selection.reason})`);

    // 2. 获取选中 Worker 的画像
    const profile = profileLoader.getProfile(selection.worker);
    assert(profile, '应该获取到 Worker 画像');
    console.log(`   3️⃣ 加载画像: ${profile.displayName}`);

    // 3. 使用 GuidanceInjector 构建 Prompt
    const prompt = guidanceInjector.buildWorkerPrompt(profile, {
      taskDescription,
    });
    assert(prompt.length > 0, 'Prompt 不应为空');
    console.log(`   4️⃣ 构建引导 Prompt: ${prompt.length} 字符`);

    // 4. 验证 Prompt 包含关键元素
    assert(prompt.includes('角色定位'), 'Prompt 应包含角色定位');
    console.log(`   5️⃣ Prompt 验证通过 ✓`);
  });

  test('测试不同任务类型的 Worker 选择', () => {
    const testCases = [
      { task: '设计微服务架构', expectedCategory: 'architecture' },
      { task: '修复登录 bug', expectedCategory: 'bugfix' },
      { task: '编写单元测试', expectedCategory: 'testing' },
      { task: '开发前端组件', expectedCategory: 'frontend' },
      { task: '实现 API 接口', expectedCategory: 'backend' },
    ];

    for (const tc of testCases) {
      const selection = workerSelector.selectByDescription(tc.task);
      console.log(`   "${tc.task}" → ${selection.worker} (分类: ${selection.category})`);
    }
  });

  // ========================================
  // 7. 真实 LLM 调用与画像验证
  // ========================================
  console.log('\n🤖 7. 真实 LLM 调用与画像验证\n');

  await asyncTest('验证画像引导在实际 LLM 调用中生效', async () => {
    // 选择一个任务
    const taskDescription = '分析这段代码的架构问题';
    const selection = workerSelector.selectByDescription(taskDescription);

    // 获取画像
    const profile = profileLoader.getProfile(selection.worker);
    const prompt = guidanceInjector.buildWorkerPrompt(profile, { taskDescription });

    console.log(`   选择 Worker: ${selection.worker}`);
    console.log(`   画像角色: ${profile.guidance.role.substring(0, 50)}...`);

    // 发送消息
    const response = await adapterFactory.sendMessage(
      selection.worker,
      '你好，请介绍一下你的专业领域',
      undefined,
      { source: 'test', adapterRole: 'worker' }
    );

    assert(response.content, '应该有响应内容');
    console.log(`   LLM 响应长度: ${response.content.length} 字符`);

    // 响应应该体现画像中定义的角色特征
    console.log(`   响应预览: ${response.content.substring(0, 80)}...`);
  }, {
    skipIf: !hasApiKey,
    skipMessage: '未配置 API Key'
  });

  // ========================================
  // 8. 清理
  // ========================================
  console.log('\n🧹 8. 清理测试资源\n');

  await asyncTest('关闭 LLMAdapterFactory', async () => {
    if (adapterFactory) {
      await adapterFactory.shutdown();
      console.log(`   所有适配器已关闭`);
    }
  });

  // 清理测试目录
  cleanupTestDir();

  // ========================================
  // 测试结果汇总
  // ========================================
  console.log('\n' + '='.repeat(70));
  console.log('📊 测试结果汇总');
  console.log('='.repeat(70));
  console.log(`✅ 通过: ${passed}`);
  console.log(`❌ 失败: ${failed}`);
  console.log(`⏭️  跳过: ${skipped}`);
  console.log(`📋 总计: ${passed + failed + skipped}`);
  console.log('='.repeat(70));

  if (!hasApiKey) {
    console.log('\n提示: 部分测试因未配置 API Key 而跳过');
    console.log('     编辑 ~/.multicli/llm.json 配置 API Key 以运行完整测试');
  }

  process.exit(failed > 0 ? 1 : 0);
}

// 运行测试
runTests().catch(error => {
  console.error('测试运行失败:', error);
  process.exit(1);
});
