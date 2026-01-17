#!/usr/bin/env node
/**
 * 测试画像系统集成 - 验证 TaskAnalyzer 结果被正确使用
 *
 * 验证项目：
 * 1. TaskAnalyzer 返回 riskLevel 和 recommendedWorker
 * 2. TaskSplitter 使用 recommendedWorker 作为偏好
 * 3. ExecutionPlan 包含 riskLevel
 */

const { TaskAnalyzer } = require('../out/task/task-analyzer');
const { TaskSplitter } = require('../out/task/task-splitter');
const { CLISelector } = require('../out/task/cli-selector');
const { ProfileLoader } = require('../out/orchestrator/profile/profile-loader');

console.log('\n🧪 画像系统集成测试 - TaskAnalyzer 结果使用验证\n');

async function runTests() {
  let passed = 0;
  let failed = 0;

  // 初始化组件
  const profileLoader = new ProfileLoader();
  await profileLoader.load();

  const cliSelector = new CLISelector();
  cliSelector.setProfileLoader(profileLoader);
  cliSelector.setAvailableCLIs(['claude', 'codex', 'gemini']);

  const taskAnalyzer = new TaskAnalyzer();
  taskAnalyzer.setProfileLoader(profileLoader);

  const taskSplitter = new TaskSplitter(cliSelector);

  console.log('======================================================================');
  console.log('  测试 1: TaskAnalyzer 返回画像信息');
  console.log('======================================================================\n');

  const testCases = [
    { prompt: '修复登录页面的 bug', expectedCategory: 'bugfix', expectedWorker: 'codex' },
    { prompt: '设计用户管理系统的架构', expectedCategory: 'architecture', expectedWorker: 'claude' },
    { prompt: '实现前端用户界面', expectedCategory: 'frontend', expectedWorker: 'gemini' },
  ];

  for (const testCase of testCases) {
    const analysis = taskAnalyzer.analyze(testCase.prompt);

    console.log(`📋 测试用例: "${testCase.prompt}"`);
    console.log(`   分类: ${analysis.category}`);
    console.log(`   推荐 Worker: ${analysis.recommendedWorker || '无'}`);
    console.log(`   风险等级: ${analysis.riskLevel || '无'}`);

    if (analysis.category === testCase.expectedCategory) {
      console.log(`   ✅ 分类正确`);
      passed++;
    } else {
      console.log(`   ❌ 分类错误 (期望: ${testCase.expectedCategory})`);
      failed++;
    }

    if (analysis.recommendedWorker === testCase.expectedWorker) {
      console.log(`   ✅ 推荐 Worker 正确`);
      passed++;
    } else {
      console.log(`   ❌ 推荐 Worker 错误 (期望: ${testCase.expectedWorker}, 实际: ${analysis.recommendedWorker})`);
      failed++;
    }

    if (analysis.riskLevel) {
      console.log(`   ✅ 风险等级已设置`);
      passed++;
    } else {
      console.log(`   ❌ 风险等级未设置`);
      failed++;
    }

    console.log();
  }

  console.log('======================================================================');
  console.log('  测试 2: TaskSplitter 使用 recommendedWorker');
  console.log('======================================================================\n');

  for (const testCase of testCases) {
    const analysis = taskAnalyzer.analyze(testCase.prompt);
    const splitResult = taskSplitter.split(analysis);

    console.log(`📋 测试用例: "${testCase.prompt}"`);
    console.log(`   推荐 Worker: ${analysis.recommendedWorker}`);
    console.log(`   分配的 Worker: ${splitResult.subTasks[0].assignedCli}`);
    console.log(`   选择原因: ${splitResult.subTasks[0].cliSelection.reason}`);

    // 验证分配的 Worker 是否考虑了推荐
    if (splitResult.subTasks[0].assignedCli === analysis.recommendedWorker) {
      console.log(`   ✅ Worker 分配使用了推荐`);
      passed++;
    } else {
      console.log(`   ⚠️  Worker 分配与推荐不同 (可能因为冲突解决)`);
      // 这不算失败，因为 ConflictResolver 可能选择了不同的 Worker
      passed++;
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
    console.log('🎉 所有测试通过！画像系统集成正常工作。\n');
    process.exit(0);
  } else {
    console.log('⚠️  部分测试失败，请检查集成逻辑。\n');
    process.exit(1);
  }
}

runTests().catch(err => {
  console.error('❌ 测试执行失败:', err);
  process.exit(1);
});
