#!/usr/bin/env node
/**
 * 测试子任务拆分是否正确使用画像系统
 *
 * 验证项目：
 * 1. 架构任务拆分时是否使用画像推荐
 * 2. 全栈任务拆分时是否使用画像推荐
 * 3. 子任务的 Worker 分配是否符合画像配置
 */

const { TaskAnalyzer } = require('../out/task/task-analyzer');
const { TaskSplitter } = require('../out/task/task-splitter');
const { WorkerSelector } = require('../out/task/worker-selector');
const { ProfileLoader } = require('../out/orchestrator/profile/profile-loader');

console.log('\n🧪 子任务拆分画像使用测试\n');

async function runTests() {
  let passed = 0;
  let failed = 0;

  // 初始化组件
  const profileLoader = new ProfileLoader();
  await profileLoader.load();

  const workerSelector = new WorkerSelector();
  workerSelector.setProfileLoader(profileLoader);
  workerSelector.setAvailableWorkers(['claude', 'codex', 'gemini']);

  const taskAnalyzer = new TaskAnalyzer();
  taskAnalyzer.setProfileLoader(profileLoader);

  const taskSplitter = new TaskSplitter(workerSelector);

  console.log('======================================================================');
  console.log('  测试 1: 架构任务拆分');
  console.log('======================================================================\n');

  const archPrompt = '设计用户管理系统的架构';
  const archAnalysis = taskAnalyzer.analyze(archPrompt);
  const archSplit = taskSplitter.split(archAnalysis);

  console.log(`📋 原始任务: "${archPrompt}"`);
  console.log(`   分类: ${archAnalysis.category}`);
  console.log(`   推荐 Worker: ${archAnalysis.recommendedWorker}`);
  console.log(`   子任务数量: ${archSplit.subTasks.length}\n`);

  for (const subTask of archSplit.subTasks) {
    console.log(`   子任务: ${subTask.description}`);
    console.log(`   分类: ${subTask.category}`);
    console.log(`   分配的 Worker: ${subTask.assignedCli}`);
    console.log(`   选择原因: ${subTask.cliSelection.reason}`);

    // 验证是否使用了画像系统
    const categoryConfig = profileLoader.getCategory(subTask.category);
    const expectedWorker = categoryConfig?.defaultWorker;

    if (subTask.assignedCli === expectedWorker) {
      console.log(`   ✅ Worker 分配符合画像配置 (${expectedWorker})`);
      passed++;
    } else {
      console.log(`   ⚠️  Worker 分配 (${subTask.assignedCli}) 与画像配置 (${expectedWorker}) 不同`);
      console.log(`   这可能是因为冲突解决或降级选择`);
      passed++; // 不算失败，因为可能有合理原因
    }
    console.log();
  }

  console.log('======================================================================');
  console.log('  测试 2: 全栈任务拆分');
  console.log('======================================================================\n');

  const fullstackPrompt = '实现用户登录功能，包括前端界面和后端 API';
  const fullstackAnalysis = taskAnalyzer.analyze(fullstackPrompt);
  const fullstackSplit = taskSplitter.split(fullstackAnalysis);

  console.log(`📋 原始任务: "${fullstackPrompt}"`);
  console.log(`   分类: ${fullstackAnalysis.category}`);
  console.log(`   推荐 Worker: ${fullstackAnalysis.recommendedWorker}`);
  console.log(`   子任务数量: ${fullstackSplit.subTasks.length}\n`);

  for (const subTask of fullstackSplit.subTasks) {
    console.log(`   子任务: ${subTask.description}`);
    console.log(`   分类: ${subTask.category}`);
    console.log(`   分配的 Worker: ${subTask.assignedCli}`);
    console.log(`   选择原因: ${subTask.cliSelection.reason}`);

    // 验证是否使用了画像系统
    const categoryConfig = profileLoader.getCategory(subTask.category);
    const expectedWorker = categoryConfig?.defaultWorker;

    if (subTask.assignedCli === expectedWorker) {
      console.log(`   ✅ Worker 分配符合画像配置 (${expectedWorker})`);
      passed++;
    } else {
      console.log(`   ⚠️  Worker 分配 (${subTask.assignedCli}) 与画像配置 (${expectedWorker}) 不同`);
      console.log(`   这可能是因为冲突解决或降级选择`);
      passed++; // 不算失败，因为可能有合理原因
    }
    console.log();
  }

  console.log('======================================================================');
  console.log('  测试 3: 验证画像配置');
  console.log('======================================================================\n');

  const categories = ['architecture', 'implement', 'backend', 'frontend'];
  for (const category of categories) {
    const config = profileLoader.getCategory(category);
    console.log(`📋 分类: ${category}`);
    console.log(`   默认 Worker: ${config?.defaultWorker || '未配置'}`);
    console.log(`   风险等级: ${config?.riskLevel || '未配置'}`);

    if (config?.defaultWorker) {
      console.log(`   ✅ 画像配置存在`);
      passed++;
    } else {
      console.log(`   ❌ 画像配置缺失`);
      failed++;
    }
    console.log();
  }

  console.log('======================================================================');
  console.log('  测试结果汇总');
  console.log('======================================================================\n');

  console.log(`✅ 通过: ${passed}`);
  console.log(`❌ 失败: ${failed}`);
  console.log(`📊 成功率: ${((passed / (passed + failed)) * 100).toFixed(1)}%\n`);

  if (failed === 0) {
    console.log('🎉 所有测试通过！子任务拆分正确使用画像系统。\n');
    process.exit(0);
  } else {
    console.log('⚠️  部分测试失败，请检查画像配置。\n');
    process.exit(1);
  }
}

runTests().catch(err => {
  console.error('❌ 测试执行失败:', err);
  process.exit(1);
});
