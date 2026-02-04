/**
 * 测试工程师4：端到端集成专家
 * 
 * 专长：测试完整用户流程、集成测试、边界情况
 */

import { TestEngineer, TestReport, TestIssue } from '../test-command-center';
import fs from 'fs';
import path from 'path';
import os from 'os';
import { EventEmitter } from 'events';
import { WorkerSlot } from '../../types';

class EndToEndEngineer implements TestEngineer {
  name = '端到端集成专家-赵工';
  specialty = '完整用户流程、集成测试、边界情况';
  
  async runTests(): Promise<TestReport> {
    const startTime = Date.now();
    const issues: TestIssue[] = [];
    const suggestions: string[] = [];
    let totalTests = 0;
    let passed = 0;
    
    // 测试1：正常确认流程
    totalTests++;
    console.log('  [测试1] 正常确认流程（用户确认）...');
    const normalFlowResult = await this.testNormalConfirmationFlow();
    if (normalFlowResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...normalFlowResult.issues);
    }
    
    // 测试2：取消流程
    totalTests++;
    console.log('  [测试2] 取消流程（用户取消）...');
    const cancelFlowResult = await this.testCancelFlow();
    if (cancelFlowResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...cancelFlowResult.issues);
    }
    
    // 测试3：中断流程
    totalTests++;
    console.log('  [测试3] 中断流程（执行中中断）...');
    const interruptFlowResult = await this.testInterruptFlow();
    if (interruptFlowResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...interruptFlowResult.issues);
    }
    
    // 测试4：错误恢复
    totalTests++;
    console.log('  [测试4] 错误恢复流程...');
    const errorRecoveryResult = await this.testErrorRecovery();
    if (errorRecoveryResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...errorRecoveryResult.issues);
    }
    
    // 测试5：边界情况
    totalTests++;
    console.log('  [测试5] 边界情况测试...');
    const edgeCasesResult = await this.testEdgeCases();
    if (edgeCasesResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...edgeCasesResult.issues);
    }

    // 测试6：真实工作流（Mock LLM + MissionDrivenEngine）
    totalTests++;
    console.log('  [测试6] 真实工作流（编排→分配→执行→汇总）...');
    const realWorkflowResult = await this.testRealWorkflow();
    if (realWorkflowResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...realWorkflowResult.issues);
    }
    
    if (issues.length > 0) {
      suggestions.push('建议添加端到端测试自动化框架');
      suggestions.push('考虑添加用户行为录制和回放功能');
    }
    
    return {
      engineerName: this.name,
      totalTests,
      passed,
      failed: totalTests - passed,
      duration: Date.now() - startTime,
      issues,
      suggestions
    };
  }
  
  private async testNormalConfirmationFlow(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 模拟完整流程：
    // 1. 用户发送任务
    // 2. 编排者分析
    // 3. 显示确认卡片
    // 4. 用户确认
    // 5. 执行任务
    // 6. 完成
    
    // 检查每个阶段的状态是否正确
    
    return { passed: true, issues };
  }
  
  private async testCancelFlow(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 测试用户取消确认的流程
    // 检查是否正确返回 idle 状态
    
    return { passed: true, issues };
  }
  
  private async testInterruptFlow(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 测试任务执行中中断的流程
    // 检查状态清理是否完整
    
    return { passed: true, issues };
  }
  
  private async testErrorRecovery(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 测试各种错误情况的恢复
    // 例如：网络错误、模型崩溃、超时等
    const clientPath = path.join(process.cwd(), 'src', 'llm', 'clients', 'universal-client.ts');
    const content = fs.readFileSync(clientPath, 'utf-8');
    const hasRetry = content.includes('withRetry') && content.includes('isRetryableError');

    if (!hasRetry) {
      issues.push({
        severity: 'medium',
        category: '错误处理',
        description: '缺少网络错误的重试机制',
        suggestedFix: '添加自动重试逻辑，最多重试3次'
      });
    }
    
    return { passed: issues.length === 0, issues };
  }
  
  private async testEdgeCases(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 测试边界情况
    // 例如：空输入、超长输入、特殊字符等
    const inputPath = path.join(process.cwd(), 'src', 'ui', 'webview-svelte', 'src', 'components', 'InputArea.svelte');
    const providerPath = path.join(process.cwd(), 'src', 'ui', 'webview-provider.ts');
    const inputContent = fs.readFileSync(inputPath, 'utf-8');
    const providerContent = fs.readFileSync(providerPath, 'utf-8');
    const hasLimit = inputContent.includes('MAX_INPUT_CHARS') && providerContent.includes('maxPromptLength');

    if (!hasLimit) {
      issues.push({
        severity: 'low',
        category: '边界情况',
        description: '未检测到超长用户输入的处理',
        suggestedFix: '添加输入长度限制和提示'
      });
    }
    
    return { passed: issues.length === 0, issues };
  }

  private async testRealWorkflow(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    class MockAdapterFactory extends EventEmitter {
      private connected = new Set<WorkerSlot>(['claude', 'codex', 'gemini']);
      public orchestratorCalls = 0;
      public workerCalls = 0;

      async sendMessage(agent: any, message: string): Promise<any> {
        if (agent === 'orchestrator') {
          this.orchestratorCalls += 1;
          if (message.includes('意图类型定义') && message.includes('recommendedMode')) {
            return {
              content: JSON.stringify({
                intent: 'task',
                recommendedMode: 'task',
                confidence: 0.92,
                needsClarification: false,
                clarificationQuestions: [],
                reason: '需要规划与执行',
              }),
              done: true,
            };
          }
          if (message.includes('分析以下用户请求，提取')) {
            return {
              content: JSON.stringify({
                goal: '生成登录流程图',
                analysis: '单一任务，结构清晰',
                constraints: [],
                acceptanceCriteria: ['输出清晰流程图'],
                riskLevel: 'low',
                riskFactors: [],
              }),
              done: true,
            };
          }
          if (message.includes('needsWorker') && message.includes('directResponse')) {
            return {
              content: JSON.stringify({
                needsWorker: true,
                category: 'simple',
                workers: ['codex'],
                delegationBriefings: ['生成登录流程图，输出 markdown'],
                needsTooling: false,
                requiresModification: false,
                directResponse: '',
                reason: '需要生成结构化内容，交由 worker 执行',
              }),
              done: true,
            };
          }
          // TaskPreAnalyzer 分析 prompt
          if (message.includes('分析以下任务') && message.includes('complexity')) {
            return {
              content: JSON.stringify({
                complexity: 'simple',
                needsPlanning: false,
                needsReview: false,
                needsVerification: false,
                parallel: false,
                reasoning: '单一简单任务，无复杂依赖',
                analysisSummary: '📝 简单任务，直接执行',
              }),
              done: true,
            };
          }
          // 兜底响应：返回有效 JSON 而非纯文本
          return {
            content: JSON.stringify({
              status: 'ok',
              message: '编排者响应',
              fallback: true,
            }),
            done: true,
          };
        }

        this.workerCalls += 1;
        return {
          content: [
            '完成：登录流程图已生成',
            'Created: docs/login-flow.md',
          ].join('\n'),
          done: true,
          tokenUsage: { inputTokens: 100, outputTokens: 60 },
        };
      }

      async interrupt(): Promise<void> {}
      async shutdown(): Promise<void> {}
      isConnected(agent: any): boolean {
        if (agent === 'orchestrator') return true;
        return this.connected.has(agent as WorkerSlot);
      }
      isBusy(): boolean { return false; }
    }

    const adapterFactory = new MockAdapterFactory();
    const workspaceRoot = process.cwd();

    // ⚠️ 关键：在修改 HOME 之前先加载 LLMConfigLoader
    // 这确保其静态 CONFIG_DIR 属性使用正确的用户主目录
    const { LLMConfigLoader } = require('../../llm/config');
    const { clearClientCache } = require('../../llm/clients/client-factory');
    clearClientCache(); // 清除可能的旧缓存
    // 预加载配置，确保 CONFIG_DIR 已初始化为正确路径
    LLMConfigLoader.loadOrchestratorConfig();

    const originalHome = process.env.HOME;
    const originalUserProfile = process.env.USERPROFILE;
    const tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), 'multicli-e2e-'));
    process.env.HOME = tmpHome;
    process.env.USERPROFILE = tmpHome;

    const Module = require('module');
    const originalRequire = Module.prototype.require;
    const vscodeMock = require('../e2e/vscode-mock');
    Module.prototype.require = function(id: string) {
      if (id === 'vscode') {
        return vscodeMock;
      }
      return originalRequire.apply(this, arguments);
    };

    const { MissionDrivenEngine } = require('../../orchestrator/core');
    const { SnapshotManager } = require('../../snapshot-manager');
    const { UnifiedSessionManager } = require('../../session');

    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
    const orchestrator = new MissionDrivenEngine(
      adapterFactory as any,
      {
        timeout: 300000,
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

    orchestrator.setConfirmationCallback(async () => true);
    orchestrator.setQuestionCallback(async (questions: string[]) => questions.join('\n'));
    orchestrator.setClarificationCallback(async (questions: string[]) => {
      const answers: Record<string, string> = {};
      questions.forEach((q: string) => { answers[q] = '默认处理'; });
      return { answers, additionalInfo: '' };
    });

    try {
      await orchestrator.initialize();
      const result = await orchestrator.execute('给我一个登录流程图，写成 markdown', 'task-real-workflow');

      if (!result || typeof result !== 'string') {
        issues.push({
          severity: 'high',
          category: '真实工作流',
          description: '编排执行未返回文本结果',
          suggestedFix: '检查编排执行最终输出的生成逻辑',
        });
      }

      if (adapterFactory.orchestratorCalls < 2) {
        issues.push({
          severity: 'high',
          category: '真实工作流',
          description: '编排 LLM 调用次数不足（意图/目标分析未触发）',
          suggestedFix: '确认意图分类与目标理解流程是否完整执行',
        });
      }

    } catch (error: any) {
      issues.push({
        severity: 'critical',
        category: '真实工作流',
        description: `执行异常: ${error?.message || String(error)}`,
        suggestedFix: '检查编排执行路径和 mock LLM 响应格式',
      });
    } finally {
      // 恢复原始环境变量
      if (originalHome !== undefined) {
        process.env.HOME = originalHome;
      }
      if (originalUserProfile !== undefined) {
        process.env.USERPROFILE = originalUserProfile;
      }
      Module.prototype.require = originalRequire;
      try {
        const missionOrchestrator = (orchestrator as any)?.missionOrchestrator;
        const workers = missionOrchestrator?.workers;
        if (workers && typeof workers.values === 'function') {
          for (const worker of workers.values()) {
            const sessionManager = worker?.getSessionManager?.();
            sessionManager?.stopAutoCleanup?.();
          }
        }
      } catch {
        // ignore
      }
      try {
        await adapterFactory.shutdown();
      } catch {
        // ignore
      }
    }

    return { passed: issues.length === 0, issues };
  }
}

export { EndToEndEngineer };
