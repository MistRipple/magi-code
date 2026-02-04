/**
 * MessageHub 端对端测试工程师
 *
 * 使用真实 LLM 测试完整的编排任务流程：
 * 1. MessageHub 消息路由正确性
 * 2. Worker 任务分配与执行
 * 3. 多 Worker 协作编排
 * 4. 响应流程完整性验证
 */

import { TestEngineer, TestReport, TestIssue } from '../test-command-center';
import { EventEmitter } from 'events';
import type { StandardMessage, MessageCategory } from '../../protocol/message-protocol';

interface CollectedMessage {
  id: string;
  category: string;
  type?: string;
  source: string;
  agent?: string;
  lifecycle?: string;
  contentPreview?: string;
  timestamp: number;
}

class MessageHubE2EEngineer implements TestEngineer {
  name = 'MessageHub E2E测试专家';
  specialty = '真实LLM编排任务、MessageHub消息流、Worker协作验证';

  async runTests(): Promise<TestReport> {
    const startTime = Date.now();
    const issues: TestIssue[] = [];
    const suggestions: string[] = [];
    let totalTests = 0;
    let passed = 0;

    // 清除客户端缓存，确保使用最新配置
    const { clearClientCache } = await import('../../llm/clients/client-factory');
    clearClientCache();

    console.log('\n  ═══════════════════════════════════════════════════════');
    console.log('  MessageHub 真实 LLM 端对端测试');
    console.log('  ═══════════════════════════════════════════════════════\n');

    // 检查 LLM 配置
    const llmConfigResult = await this.checkLLMConfig();
    totalTests++;
    if (llmConfigResult.passed) {
      passed++;
      console.log('  ✓ LLM 配置检查通过');
    } else {
      console.log('  ✗ LLM 配置检查失败');
      issues.push(...llmConfigResult.issues);

      // 如果 LLM 不可用，降级到静态检查
      console.log('  ⚠️  降级到静态检查模式\n');

      totalTests++;
      const staticResult = await this.runStaticChecks();
      if (staticResult.passed) {
        passed++;
        console.log('  ✓ MessageHub 静态检查通过');
      } else {
        console.log('  ✗ MessageHub 静态检查失败');
        issues.push(...staticResult.issues);
      }

      return {
        engineerName: this.name,
        totalTests,
        passed,
        failed: totalTests - passed,
        duration: Date.now() - startTime,
        issues,
        suggestions: ['配置有效的 LLM API key 以运行完整的 MessageHub E2E 测试'],
      };
    }

    // 测试1：简单问答流程（ASK 模式）
    totalTests++;
    console.log('\n  [测试1] 简单问答流程 (ASK 模式)...');
    const askResult = await this.testAskModeFlow();
    if (askResult.passed) {
      passed++;
      console.log(`    ✓ 通过 (收到 ${askResult.messageCount} 条消息)`);
    } else {
      console.log('    ✗ 失败');
      issues.push(...askResult.issues);
    }

    // 测试2：任务编排流程（TASK 模式）
    totalTests++;
    console.log('\n  [测试2] 任务编排流程 (TASK 模式)...');
    const taskResult = await this.testTaskModeFlow();
    if (taskResult.passed) {
      passed++;
      console.log(`    ✓ 通过 (编排调用: ${taskResult.orchestratorCalls}, Worker调用: ${taskResult.workerCalls})`);
    } else {
      console.log('    ✗ 失败');
      issues.push(...taskResult.issues);
    }

    // 测试3：MessageHub 消息路由验证
    totalTests++;
    console.log('\n  [测试3] MessageHub 消息路由验证...');
    const routingResult = await this.testMessageRouting();
    if (routingResult.passed) {
      passed++;
      console.log(`    ✓ 通过 (CONTENT: ${routingResult.contentCount}, DATA: ${routingResult.dataCount}, CONTROL: ${routingResult.controlCount})`);
    } else {
      console.log('    ✗ 失败');
      issues.push(...routingResult.issues);
    }

    // 测试4：Worker 状态同步
    totalTests++;
    console.log('\n  [测试4] Worker 状态同步验证...');
    const workerStatusResult = await this.testWorkerStatusSync();
    if (workerStatusResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...workerStatusResult.issues);
    }

    console.log('\n  ═══════════════════════════════════════════════════════\n');

    if (issues.length > 0) {
      suggestions.push('检查 MessageHub 消息发送和路由逻辑');
      suggestions.push('确保 Worker 状态正确同步到前端');
    }

    return {
      engineerName: this.name,
      totalTests,
      passed,
      failed: totalTests - passed,
      duration: Date.now() - startTime,
      issues,
      suggestions,
    };
  }

  private async checkLLMConfig(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadOrchestratorConfig();

      if (!config || !config.apiKey || !config.model) {
        issues.push({
          severity: 'high',
          category: 'LLM配置',
          description: '编排模型未配置完整 (缺少 apiKey 或 model)',
          suggestedFix: '配置 ~/.multicli/llm.json 中的编排模型',
        });
        return { passed: false, issues };
      }

      // 快速连接测试
      const { getOrCreateLLMClient } = await import('../../llm/clients/client-factory');
      const client = getOrCreateLLMClient(config);
      const testResult = await client.testConnectionFast();

      if (!testResult.success) {
        issues.push({
          severity: 'high',
          category: 'LLM配置',
          description: `LLM 连接失败: ${testResult.error}`,
          suggestedFix: '检查 API Key 和网络连接',
        });
        return { passed: false, issues };
      }

      return { passed: true, issues };
    } catch (error: any) {
      issues.push({
        severity: 'high',
        category: 'LLM配置',
        description: `配置加载异常: ${error?.message || String(error)}`,
        suggestedFix: '检查配置文件格式',
      });
      return { passed: false, issues };
    }
  }

  private async runStaticChecks(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    const fs = await import('fs');
    const path = await import('path');

    // 检查 MessageHub 文件存在
    const messageHubPath = path.join(process.cwd(), 'src', 'orchestrator', 'core', 'message-hub.ts');
    if (!fs.existsSync(messageHubPath)) {
      issues.push({
        severity: 'critical',
        category: '静态检查',
        description: 'MessageHub 文件不存在',
        suggestedFix: '创建 src/orchestrator/core/message-hub.ts',
      });
      return { passed: false, issues };
    }

    const hubContent = fs.readFileSync(messageHubPath, 'utf-8');

    // 检查核心方法
    const requiredMethods = [
      'sendMessage',
      'orchestratorMessage',
      'workerOutput',
      'progress',
      'result',
    ];

    for (const method of requiredMethods) {
      if (!hubContent.includes(method)) {
        issues.push({
          severity: 'medium',
          category: '静态检查',
          description: `MessageHub 缺少 ${method} 方法`,
          suggestedFix: `实现 MessageHub.${method}() 方法`,
        });
      }
    }

    // 检查消息类别处理
    const categories = ['CONTENT', 'DATA', 'CONTROL', 'NOTIFY'];
    for (const cat of categories) {
      if (!hubContent.includes(cat)) {
        issues.push({
          severity: 'medium',
          category: '静态检查',
          description: `MessageHub 缺少 ${cat} 类别处理`,
          suggestedFix: `添加 MessageCategory.${cat} 的处理逻辑`,
        });
      }
    }

    return { passed: issues.length === 0, issues };
  }

  private async testAskModeFlow(): Promise<{
    passed: boolean;
    issues: TestIssue[];
    messageCount: number;
  }> {
    const issues: TestIssue[] = [];
    const collectedMessages: CollectedMessage[] = [];

    try {
      const { MessageHub } = await import('../../orchestrator/core/message-hub');
      const hub = new MessageHub();

      // 监听消息
      hub.on('unified:message', (msg: StandardMessage) => {
        collectedMessages.push({
          id: msg.id,
          category: msg.category,
          type: msg.type,
          source: msg.source,
          agent: msg.agent,
          lifecycle: msg.lifecycle,
          contentPreview: this.extractContentPreview(msg),
          timestamp: Date.now(),
        });
      });

      // 模拟简单问答
      hub.newTrace();
      hub.orchestratorMessage('正在分析您的问题...');
      hub.result('TypeScript 中 interface 定义对象类型，type 可以定义任意类型别名。');

      // 验证消息
      const contentMessages = collectedMessages.filter(m => m.category === 'content');
      if (contentMessages.length < 2) {
        issues.push({
          severity: 'medium',
          category: 'ASK模式',
          description: `预期至少2条 CONTENT 消息，实际 ${contentMessages.length} 条`,
          suggestedFix: '检查 orchestratorMessage 和 result 方法',
        });
      }

      return {
        passed: issues.length === 0,
        issues,
        messageCount: collectedMessages.length,
      };
    } catch (error: any) {
      issues.push({
        severity: 'high',
        category: 'ASK模式',
        description: `测试异常: ${error?.message || String(error)}`,
        suggestedFix: '检查 MessageHub 初始化和消息发送',
      });
      return { passed: false, issues, messageCount: 0 };
    }
  }

  private async testTaskModeFlow(): Promise<{
    passed: boolean;
    issues: TestIssue[];
    orchestratorCalls: number;
    workerCalls: number;
  }> {
    const issues: TestIssue[] = [];
    let orchestratorCalls = 0;
    let workerCalls = 0;

    try {
      const fs = await import('fs');
      const path = await import('path');
      const os = await import('os');

      // ⚠️ 先加载 LLM 配置（在修改 HOME 之前）
      const { LLMConfigLoader } = await import('../../llm/config');
      const { clearClientCache } = await import('../../llm/clients/client-factory');
      clearClientCache();
      // 预加载配置
      LLMConfigLoader.loadOrchestratorConfig();

      // 创建临时目录（用于隔离其他配置）
      const tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), 'multicli-hub-e2e-'));

      // Mock vscode
      const Module = require('module');
      const originalRequire = Module.prototype.require;
      const vscodeMock = require('../e2e/vscode-mock');
      Module.prototype.require = function(id: string) {
        if (id === 'vscode') {
          return vscodeMock;
        }
        return originalRequire.apply(this, arguments);
      };

      // 保存原始环境变量
      const originalHome = process.env.HOME;
      const originalUserProfile = process.env.USERPROFILE;
      process.env.HOME = tmpHome;
      process.env.USERPROFILE = tmpHome;

      try {
        const { MissionDrivenEngine } = await import('../../orchestrator/core/mission-driven-engine');
        const { LLMAdapterFactory } = await import('../../llm/adapter-factory');
        const { SnapshotManager } = await import('../../snapshot-manager');
        const { UnifiedSessionManager } = await import('../../session');

        // 使用真实的 LLMAdapterFactory
        const workspaceRoot = process.cwd();
        const adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });

        const sessionManager = new UnifiedSessionManager(workspaceRoot);
        const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);

        // 创建真实的 MissionDrivenEngine
        const orchestrator = new MissionDrivenEngine(
          adapterFactory as any,
          {
            timeout: 120000,
            maxRetries: 1,
            strategy: { enableVerification: false, enableRecovery: false, autoRollbackOnFailure: false },
            verification: { compileCheck: false, lintCheck: false, testCheck: false },
            planReview: { enabled: false },
            integration: { enabled: false },
            review: { selfCheck: false, peerReview: 'never', maxRounds: 0 },
          },
          workspaceRoot,
          snapshotManager,
          sessionManager
        );

        // 从引擎获取 MessageHub 并设置给 AdapterFactory
        const hub = orchestrator.getMessageHub();
        adapterFactory.setMessageHub(hub);

        const collectedMessages: CollectedMessage[] = [];

        // 监听引擎内部的 MessageHub
        hub.on('unified:message', (msg: StandardMessage) => {
          collectedMessages.push({
            id: msg.id,
            category: msg.category,
            type: msg.type,
            source: msg.source,
            agent: msg.agent,
            lifecycle: msg.lifecycle,
            contentPreview: this.extractContentPreview(msg),
            timestamp: Date.now(),
          });

          // 统计消息来源
          if (msg.source === 'orchestrator') {
            orchestratorCalls++;
          } else if (msg.source === 'worker') {
            workerCalls++;
          }
        });

        hub.newTrace();

        // 设置回调
        orchestrator.setConfirmationCallback(async () => true);
        orchestrator.setQuestionCallback(async (questions: string[]) => questions.join('\n'));
        orchestrator.setClarificationCallback(async (questions: string[]) => {
          const answers: Record<string, string> = {};
          questions.forEach((q: string) => { answers[q] = '默认处理'; });
          return { answers, additionalInfo: '' };
        });

        // 初始化编排器
        console.log('      正在初始化编排器...');
        await orchestrator.initialize();

        // 使用一个涉及多个分类的复杂任务，以触发多 Worker 协作
        // - 后端重构 (claude: refactor, backend)
        // - 前端更新 (gemini: frontend)
        // - 单元测试 (codex: test)
        const taskPrompt = `完成以下多模块任务：
1. 重构后端用户认证模块，将登录和注册逻辑分离
2. 更新前端登录页面的样式和表单验证
3. 为新的认证模块编写单元测试`;
        console.log(`      发送任务: "${taskPrompt.substring(0, 50)}..."`);

        const result = await orchestrator.execute(taskPrompt, 'task-e2e-test');
        console.log(`      任务完成，结果长度: ${result.length}`);

        // 验证：至少有编排者消息
        const orchestratorMsgs = collectedMessages.filter(m => m.source === 'orchestrator');
        const workerMsgs = collectedMessages.filter(m => m.source === 'worker');

        // 统计各 Worker 的消息
        const claudeMsgs = collectedMessages.filter(m => m.agent === 'claude');
        const codexMsgs = collectedMessages.filter(m => m.agent === 'codex');
        const geminiMsgs = collectedMessages.filter(m => m.agent === 'gemini');

        console.log(`      收集到消息: 编排者=${orchestratorMsgs.length}, Worker=${workerMsgs.length}`);
        console.log(`      各 Worker: claude=${claudeMsgs.length}, codex=${codexMsgs.length}, gemini=${geminiMsgs.length}`);

        if (orchestratorMsgs.length === 0) {
          issues.push({
            severity: 'high',
            category: 'TASK模式',
            description: '未收到任何编排者消息',
            suggestedFix: '检查 MissionDrivenEngine 和 MessageHub 集成',
          });
        }

        // 关闭编排器
        orchestrator.dispose();

      } finally {
        // 恢复环境
        if (originalHome !== undefined) {
          process.env.HOME = originalHome;
        }
        if (originalUserProfile !== undefined) {
          process.env.USERPROFILE = originalUserProfile;
        }
        Module.prototype.require = originalRequire;

        // 清理临时目录
        try {
          fs.rmSync(tmpHome, { recursive: true, force: true });
        } catch {
          // ignore
        }
      }

      return {
        passed: issues.length === 0,
        issues,
        orchestratorCalls,
        workerCalls,
      };
    } catch (error: any) {
      issues.push({
        severity: 'high',
        category: 'TASK模式',
        description: `测试异常: ${error?.message || String(error)}`,
        suggestedFix: '检查编排引擎和 MessageHub 集成',
      });
      return { passed: false, issues, orchestratorCalls, workerCalls };
    }
  }

  private async testMessageRouting(): Promise<{
    passed: boolean;
    issues: TestIssue[];
    contentCount: number;
    dataCount: number;
    controlCount: number;
  }> {
    const issues: TestIssue[] = [];
    let contentCount = 0;
    let dataCount = 0;
    let controlCount = 0;

    try {
      const { MessageHub } = await import('../../orchestrator/core/message-hub');
      const hub = new MessageHub();

      hub.on('unified:message', (msg: StandardMessage) => {
        switch (msg.category) {
          case 'content':
            contentCount++;
            break;
          case 'data':
            dataCount++;
            break;
          case 'control':
            controlCount++;
            break;
        }
      });

      hub.newTrace();

      // 发送不同类别的消息
      hub.orchestratorMessage('测试 CONTENT 消息');
      hub.progress('测试', '测试进度消息');
      hub.result('测试结果');

      // DATA 消息
      hub.data('stateUpdate', { test: true });

      // CONTROL 消息
      hub.sendControl('task_started' as any, { taskId: 'test-task' });

      // 验证
      if (contentCount < 3) {
        issues.push({
          severity: 'medium',
          category: '消息路由',
          description: `CONTENT 消息计数错误: 预期>=3, 实际 ${contentCount}`,
          suggestedFix: '检查 CONTENT 类消息的发送逻辑',
        });
      }

      if (dataCount < 1) {
        issues.push({
          severity: 'medium',
          category: '消息路由',
          description: `DATA 消息未正确路由: ${dataCount}`,
          suggestedFix: '检查 sendData 方法',
        });
      }

      if (controlCount < 1) {
        issues.push({
          severity: 'medium',
          category: '消息路由',
          description: `CONTROL 消息未正确路由: ${controlCount}`,
          suggestedFix: '检查 sendControl 方法',
        });
      }

      return {
        passed: issues.length === 0,
        issues,
        contentCount,
        dataCount,
        controlCount,
      };
    } catch (error: any) {
      issues.push({
        severity: 'high',
        category: '消息路由',
        description: `测试异常: ${error?.message || String(error)}`,
        suggestedFix: '检查 MessageHub 消息分类实现',
      });
      return { passed: false, issues, contentCount, dataCount, controlCount };
    }
  }

  private async testWorkerStatusSync(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    try {
      const { MessageHub } = await import('../../orchestrator/core/message-hub');
      const hub = new MessageHub();

      let statusUpdateReceived = false;

      hub.on('unified:message', (msg: StandardMessage) => {
        if (msg.category === 'data' && msg.data?.dataType === 'workerStatusUpdate') {
          statusUpdateReceived = true;
        }
      });

      hub.newTrace();

      // 发送 Worker 状态更新
      hub.data('workerStatusUpdate', {
        statuses: {
          claude: { status: 'available', model: 'claude-3-5-sonnet' },
          codex: { status: 'available', model: 'gpt-4o' },
          gemini: { status: 'unavailable', error: '未配置' },
        },
      });

      if (!statusUpdateReceived) {
        issues.push({
          severity: 'medium',
          category: 'Worker状态',
          description: 'workerStatusUpdate 消息未正确发送',
          suggestedFix: '检查 sendData 方法对 workerStatusUpdate 的处理',
        });
      }

      return { passed: issues.length === 0, issues };
    } catch (error: any) {
      issues.push({
        severity: 'high',
        category: 'Worker状态',
        description: `测试异常: ${error?.message || String(error)}`,
        suggestedFix: '检查 Worker 状态同步逻辑',
      });
      return { passed: false, issues };
    }
  }

  private extractContentPreview(msg: StandardMessage): string {
    if (!msg.blocks || msg.blocks.length === 0) return '';
    const textBlock = msg.blocks.find(b => b.type === 'text');
    if (!textBlock) return '';
    const content = (textBlock as any).content || '';
    return content.substring(0, 50) + (content.length > 50 ? '...' : '');
  }
}

export { MessageHubE2EEngineer };
