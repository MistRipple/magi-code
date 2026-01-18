/**
 * 显式 Worker 指派保留测试
 * - 检测用户文本中的显式指派语句
 * - 确保在 preserveAssignments=true 时不覆盖 assignedWorker
 */

const { OrchestratorAgent } = require('../out/orchestrator/orchestrator-agent');
const { TestRunner } = require('./test-utils');

function main() {
  const runner = new TestRunner('显式 Worker 指派保留测试');

  try {
    const agentProto = OrchestratorAgent.prototype;

    // 1) 显式指派检测
    const detect = agentProto.hasExplicitWorkerAssignments;
    if (typeof detect !== 'function') {
      runner.logTest('检测函数存在', false, 'hasExplicitWorkerAssignments 未暴露');
    } else {
      const explicitText = '严格拆分为 3 个子任务：Claude 负责架构，Codex 负责后端，Gemini 负责前端';
      const implicitText = '可用 CLI: claude, codex, gemini';
      runner.logTest('检测显式指派', detect.call({}, explicitText) === true);
      runner.logTest('忽略仅列出 CLI', detect.call({}, implicitText) === false);
    }

    // 2) preserveAssignments=true 不覆盖
    const fakeAgent = {
      cliSelector: {
        getAvailableCLIs: () => ['claude', 'codex', 'gemini'],
        selectByDescription: () => ({ worker: 'codex', reason: 'mock' }),
      },
    };

    const planPreserve = {
      subTasks: [
        { description: '前端 UI', assignedWorker: 'gemini' },
      ],
    };
    agentProto.enforceProfileAssignments.call(fakeAgent, planPreserve, true);
    runner.logTest(
      'preserve=true 保留原指派',
      planPreserve.subTasks[0].assignedWorker === 'gemini'
    );

    // 3) preserveAssignments=false 允许覆盖
    const planOverride = {
      subTasks: [
        { description: '后端 API', assignedWorker: 'gemini' },
      ],
    };
    agentProto.enforceProfileAssignments.call(fakeAgent, planOverride, false);
    runner.logTest(
      'preserve=false 允许覆盖',
      planOverride.subTasks[0].assignedWorker === 'codex'
    );

    process.exit(runner.finish());
  } catch (error) {
    runner.log(`\n❌ 测试失败: ${error.message}`, 'red');
    console.error(error);
    process.exit(1);
  }
}

main();
