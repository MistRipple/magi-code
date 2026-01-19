/**
 * Mission-Driven Architecture 集成测试
 *
 * 验证新架构的核心流程：
 * 1. 创建 Mission
 * 2. 规划 Mission（定义契约、分配职责）
 * 3. Worker 自主规划 Todo
 * 4. 执行和评审
 */

import { ProfileLoader } from '../orchestrator/profile/profile-loader';
import { GuidanceInjector } from '../orchestrator/profile/guidance-injector';
import { MissionOrchestrator } from '../orchestrator/core/mission-orchestrator';
import { MissionStorageManager } from '../orchestrator/mission/mission-storage';
import type { Mission, Contract, Assignment } from '../orchestrator/mission/types';

/**
 * 测试结果
 */
interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
  duration: number;
}

/**
 * 运行所有测试
 */
async function runTests(): Promise<void> {
  console.log('='.repeat(60));
  console.log('Mission-Driven Architecture 集成测试');
  console.log('='.repeat(60));

  const results: TestResult[] = [];

  // 初始化依赖
  const profileLoader = new ProfileLoader();
  const guidanceInjector = new GuidanceInjector();
  const storage = new MissionStorageManager();
  const orchestrator = new MissionOrchestrator(profileLoader, guidanceInjector, storage);

  // 测试 1: 创建 Mission
  results.push(await runTest('创建 Mission', async () => {
    const mission = await orchestrator.createMission({
      userPrompt: '实现一个用户登录功能，包括 API 接口和前端表单',
      sessionId: 'test-session-1',
      context: '这是一个 TypeScript + React 项目',
    });

    if (!mission.id) throw new Error('Mission ID 未生成');
    if (mission.status !== 'draft') throw new Error(`状态错误: ${mission.status}`);
    if (mission.phase !== 'goal_understanding') throw new Error(`阶段错误: ${mission.phase}`);

    return mission;
  }));

  // 测试 2: 理解目标
  results.push(await runTest('理解目标', async () => {
    const mission = await orchestrator.createMission({
      userPrompt: '添加用户认证功能',
      sessionId: 'test-session-2',
    });

    const updated = await orchestrator.understandGoal(mission, {
      goal: '实现完整的用户登录和认证系统',
      analysis: '需要后端 API 实现登录逻辑，前端需要登录表单和状态管理',
      constraints: ['使用 JWT 进行认证', '密码需要加密存储'],
      acceptanceCriteria: ['用户可以登录', '登录状态可以持久化'],
      riskLevel: 'medium',
      riskFactors: ['涉及安全敏感操作'],
    });

    if (!updated.goal) throw new Error('目标未设置');
    if (updated.constraints.length !== 2) throw new Error('约束条件数量错误');
    if (updated.phase !== 'participant_selection') throw new Error(`阶段错误: ${updated.phase}`);

    return updated;
  }));

  // 测试 3: 选择参与者
  results.push(await runTest('选择参与者', async () => {
    const mission = await orchestrator.createMission({
      userPrompt: '实现 API 接口和架构设计',
      sessionId: 'test-session-3',
    });

    await orchestrator.understandGoal(mission, {
      goal: '实现 RESTful API 架构',
      analysis: '需要创建后端接口和进行 architecture 设计',
    });

    const participants = await orchestrator.selectParticipants(mission);

    // 如果没有自动选择，使用指定的参与者
    if (participants.length === 0) {
      const manualParticipants = await orchestrator.selectParticipants(mission, {
        preferredWorkers: ['claude'],
      });
      if (manualParticipants.length === 0) throw new Error('手动指定参与者失败');
      return manualParticipants;
    }

    if (mission.phase !== 'contract_definition') throw new Error(`阶段错误: ${mission.phase}`);

    return participants;
  }));

  // 测试 4: 完整规划流程
  results.push(await runTest('完整规划流程', async () => {
    const mission = await orchestrator.createMission({
      userPrompt: '实现用户管理模块的 API 和数据类型',
      sessionId: 'test-session-4',
    });

    const result = await orchestrator.planMission(mission, {
      goal: '创建用户管理模块',
      analysis: '需要定义用户数据类型和 CRUD API',
      constraints: ['遵循 RESTful 规范'],
      acceptanceCriteria: ['API 可以正常调用', '数据类型定义完整'],
    }, {
      participants: ['claude', 'codex'],
      requireApproval: true,
    });

    if (!result.mission) throw new Error('Mission 未返回');
    if (result.mission.status !== 'pending_approval') throw new Error(`状态错误: ${result.mission.status}`);

    // 验证契约
    console.log(`  - 生成了 ${result.contracts.length} 个契约`);

    // 验证职责分配
    if (result.assignments.length !== 2) throw new Error(`职责分配数量错误: ${result.assignments.length}`);
    console.log(`  - 分配了 ${result.assignments.length} 个职责`);

    for (const assignment of result.assignments) {
      console.log(`    - ${assignment.workerId}: ${assignment.responsibility.substring(0, 50)}...`);
    }

    return result;
  }));

  // 测试 5: Mission 生命周期
  results.push(await runTest('Mission 生命周期', async () => {
    const mission = await orchestrator.createMission({
      userPrompt: '测试生命周期',
      sessionId: 'test-session-5',
    });

    // 批准
    const approved = await orchestrator.approveMission(mission.id);
    if (approved.status !== 'executing') throw new Error('批准后状态错误');

    // 暂停
    const paused = await orchestrator.pauseMission(mission.id, '测试暂停');
    if (paused.status !== 'paused') throw new Error('暂停后状态错误');

    // 恢复
    const resumed = await orchestrator.resumeMission(mission.id);
    if (resumed.status !== 'executing') throw new Error('恢复后状态错误');

    // 完成
    const completed = await orchestrator.completeMission(mission.id);
    if (completed.status !== 'completed') throw new Error('完成后状态错误');

    return completed;
  }));

  // 测试 6: 存储和加载
  results.push(await runTest('存储和加载', async () => {
    const mission = await orchestrator.createMission({
      userPrompt: '测试存储',
      sessionId: 'test-session-6',
    });

    const loaded = await orchestrator.getMission(mission.id);
    if (!loaded) throw new Error('Mission 加载失败');
    if (loaded.id !== mission.id) throw new Error('ID 不匹配');

    const sessionMissions = await orchestrator.getSessionMissions('test-session-6');
    if (sessionMissions.length === 0) throw new Error('会话 Mission 列表为空');

    return loaded;
  }));

  // 测试 7: ProfileAwareReviewer
  results.push(await runTest('画像感知评审', async () => {
    const mission = await orchestrator.createMission({
      userPrompt: '测试评审',
      sessionId: 'test-session-7',
    });

    const result = await orchestrator.planMission(mission, {
      goal: '实现功能',
      analysis: '需要代码实现',
    }, {
      participants: ['claude'],
    });

    const reviewer = orchestrator.getReviewer();
    const reviewResult = await reviewer.reviewPlan(result.mission);

    console.log(`  - 评审通过: ${reviewResult.approved}`);
    console.log(`  - 发现 ${reviewResult.issues.length} 个问题`);
    console.log(`  - ${reviewResult.suggestions.length} 条建议`);

    return reviewResult;
  }));

  // 输出结果
  console.log('\n' + '='.repeat(60));
  console.log('测试结果汇总');
  console.log('='.repeat(60));

  let passed = 0;
  let failed = 0;

  for (const result of results) {
    const status = result.passed ? '✓' : '✗';
    const time = `${result.duration}ms`;
    console.log(`${status} ${result.name} (${time})`);
    if (result.error) {
      console.log(`  错误: ${result.error}`);
    }
    if (result.passed) passed++; else failed++;
  }

  console.log('\n' + '-'.repeat(60));
  console.log(`总计: ${results.length} 个测试, ${passed} 个通过, ${failed} 个失败`);

  if (failed > 0) {
    process.exit(1);
  }
}

/**
 * 运行单个测试
 */
async function runTest<T>(name: string, fn: () => Promise<T>): Promise<TestResult> {
  const start = Date.now();
  console.log(`\n测试: ${name}`);

  try {
    await fn();
    const duration = Date.now() - start;
    console.log(`  ✓ 通过 (${duration}ms)`);
    return { name, passed: true, duration };
  } catch (error) {
    const duration = Date.now() - start;
    const errorMessage = error instanceof Error ? error.message : String(error);
    console.log(`  ✗ 失败: ${errorMessage}`);
    return { name, passed: false, error: errorMessage, duration };
  }
}

// 运行测试
runTests().catch(console.error);
