/**
 * 画像系统集成测试脚本
 * 
 * 验证画像系统在编排流程中是否生效：
 * 1. ProfileLoader - 加载画像配置
 * 2. CLISelector - 基于画像选择 Worker
 * 3. TaskAnalyzer - 基于画像分类任务
 * 4. GuidanceInjector - 构建画像引导 Prompt
 * 5. WorkerAgent - 画像注入到任务执行
 * 
 * 运行方式：npx ts-node scripts/test-profile-system.ts
 */

import { ProfileLoader, GuidanceInjector, WorkerProfile, InjectionContext } from '../src/orchestrator/profile';
import { CLISelector } from '../src/task/cli-selector';
import { TaskAnalyzer } from '../src/task/task-analyzer';
import { WorkerType } from '../src/types';

// 测试用例定义
interface TestCase {
  name: string;
  description: string;
  expectedWorker?: WorkerType;
  expectedCategory?: string;
}

const TEST_CASES: TestCase[] = [
  { name: '代码重构', description: '重构 UserService 类，提取公共方法', expectedWorker: 'claude', expectedCategory: 'architecture' },
  { name: '修复 Bug', description: '修复登录页面的表单验证错误', expectedWorker: 'codex', expectedCategory: 'bugfix' },
  { name: '后端开发', description: '实现用户认证 API 服务和数据库操作', expectedWorker: 'claude', expectedCategory: 'backend' },
  { name: '文档编写', description: '编写项目 README 文档说明', expectedWorker: 'gemini', expectedCategory: 'docs' },
  { name: '性能优化', description: '调试排查数据库查询的 N+1 问题', expectedWorker: 'codex', expectedCategory: 'bugfix' },
  { name: '测试编写', description: '为 PaymentService 编写单元测试', expectedWorker: 'codex', expectedCategory: 'test' },
  { name: '架构设计', description: '设计微服务架构方案', expectedWorker: 'claude', expectedCategory: 'architecture' },
  { name: '前端开发', description: '实现用户界面 UI 组件和页面样式', expectedWorker: 'gemini', expectedCategory: 'frontend' },
];

// 颜色输出
const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
  dim: '\x1b[2m',
};

function log(color: keyof typeof colors, ...args: any[]) {
  console.log(colors[color], ...args, colors.reset);
}

function logSection(title: string) {
  console.log('\n' + '='.repeat(60));
  log('cyan', `  ${title}`);
  console.log('='.repeat(60));
}

async function main() {
  console.log('\n🧪 画像系统集成测试\n');

  // 1. 测试 ProfileLoader
  logSection('1. ProfileLoader 测试');
  const profileLoader = new ProfileLoader();
  await profileLoader.load();

  const profiles = profileLoader.getAllProfiles();
  log('green', `✓ 已加载 ${profiles.size} 个 Worker 画像`);

  for (const [type, profile] of profiles) {
    console.log(`  - ${type}: ${profile.displayName} (v${profile.version})`);
    console.log(`    角色: ${profile.guidance.role.slice(0, 50)}...`);
    console.log(`    优先分类: ${profile.preferences.preferredCategories.join(', ')}`);
  }

  const categories = profileLoader.getAllCategories();
  log('green', `✓ 已加载 ${categories.size} 个任务分类`);
  for (const [name, config] of categories) {
    console.log(`  - ${name}: ${config.displayName} (默认: ${config.defaultWorker})`);
  }

  // 2. 测试 CLISelector
  logSection('2. CLISelector 测试');
  const cliSelector = new CLISelector();
  cliSelector.setProfileLoader(profileLoader);
  log('green', '✓ CLISelector 已集成 ProfileLoader');

  // 3. 测试 TaskAnalyzer
  logSection('3. TaskAnalyzer 测试');
  const taskAnalyzer = new TaskAnalyzer();
  taskAnalyzer.setProfileLoader(profileLoader);
  log('green', '✓ TaskAnalyzer 已集成 ProfileLoader');

  // 4. 测试 GuidanceInjector
  logSection('4. GuidanceInjector 测试');
  const guidanceInjector = new GuidanceInjector();
  log('green', '✓ GuidanceInjector 已创建');

  // 5. 综合测试：任务分析 + Worker 选择 + Prompt 生成
  logSection('5. 综合测试：编排流程验证');

  let passed = 0;
  let failed = 0;

  for (const testCase of TEST_CASES) {
    console.log(`\n📋 测试: ${testCase.name}`);
    console.log(`   描述: ${testCase.description}`);

    // 5.1 任务分析
    const analysis = taskAnalyzer.analyze(testCase.description);
    console.log(`   分类: ${analysis.category} (复杂度: ${analysis.complexity})`);

    // 5.2 Worker 选择
    const selection = cliSelector.selectByDescription(testCase.description);
    console.log(`   选择: ${selection.worker} (分数: ${selection.score})`);
    console.log(`   原因: ${selection.reason}`);

    // 5.3 画像引导 Prompt 生成
    const profile = profileLoader.getProfile(selection.worker);
    const context: InjectionContext = {
      taskDescription: testCase.description,
      category: selection.category,
    };
    const guidance = guidanceInjector.buildWorkerPrompt(profile, context);
    console.log(`   引导 Prompt 长度: ${guidance.length} 字符`);

    // 5.4 验证结果
    const workerMatch = !testCase.expectedWorker || selection.worker === testCase.expectedWorker;
    const categoryMatch = !testCase.expectedCategory || selection.category === testCase.expectedCategory;

    if (workerMatch && categoryMatch) {
      log('green', `   ✓ 通过`);
      passed++;
    } else {
      log('red', `   ✗ 失败`);
      if (!workerMatch) log('red', `     期望 Worker: ${testCase.expectedWorker}, 实际: ${selection.worker}`);
      if (!categoryMatch) log('red', `     期望分类: ${testCase.expectedCategory}, 实际: ${selection.category}`);
      failed++;
    }
  }

  // 6. 测试结果汇总
  logSection('6. 测试结果汇总');
  console.log(`总计: ${TEST_CASES.length} 个测试`);
  log('green', `通过: ${passed}`);
  if (failed > 0) log('red', `失败: ${failed}`);
  else log('dim', `失败: ${failed}`);

  // 7. 画像 Prompt 示例输出
  logSection('7. 画像 Prompt 示例');
  const sampleProfile = profileLoader.getProfile('claude');
  const sampleContext: InjectionContext = {
    taskDescription: '重构 UserService 类，提取公共方法到 BaseService',
    targetFiles: ['src/services/user-service.ts', 'src/services/base-service.ts'],
    category: 'refactoring',
    collaborators: ['codex'],
  };
  const samplePrompt = guidanceInjector.buildFullTaskPrompt(sampleProfile, sampleContext);
  console.log(samplePrompt);

  console.log('\n' + '='.repeat(60));
  log('cyan', '  测试完成');
  console.log('='.repeat(60) + '\n');

  process.exit(failed > 0 ? 1 : 0);
}

main().catch(console.error);

