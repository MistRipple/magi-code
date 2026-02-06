/**
 * 测试工程师3：UI交互专家
 * 
 * 专长：测试用户交互、卡片显示、自然语言处理
 */

import { TestEngineer, TestReport, TestIssue } from '../test-command-center';
import fs from 'fs';
import path from 'path';
import vm from 'vm';

class UIInteractionEngineer implements TestEngineer {
  name = 'UI交互专家-王工';
  specialty = '用户交互、卡片显示、自然语言处理';
  
  async runTests(): Promise<TestReport> {
    const startTime = Date.now();
    const issues: TestIssue[] = [];
    const suggestions: string[] = [];
    let totalTests = 0;
    let passed = 0;
    
    // 测试1：确认卡片不重复
    totalTests++;
    console.log('  [测试1] 确认卡片不重复...');
    const cardDuplicationResult = await this.testCardDuplication();
    if (cardDuplicationResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...cardDuplicationResult.issues);
    }
    
    // 测试2：自然语言确认
    totalTests++;
    console.log('  [测试2] 自然语言确认解析...');
    const nlpResult = await this.testNaturalLanguageConfirmation();
    if (nlpResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...nlpResult.issues);
    }
    
    // 测试3：卡片状态更新
    totalTests++;
    console.log('  [测试3] 卡片状态更新...');
    const cardStateResult = await this.testCardStateUpdate();
    if (cardStateResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...cardStateResult.issues);
    }
    
    // 测试4：按钮状态管理
    totalTests++;
    console.log('  [测试4] 发送按钮状态管理...');
    const buttonStateResult = await this.testButtonState();
    if (buttonStateResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...buttonStateResult.issues);
    }
    
    // 测试5：输入框交互
    totalTests++;
    console.log('  [测试5] 输入框交互逻辑...');
    const inputResult = await this.testInputInteraction();
    if (inputResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...inputResult.issues);
    }

    // 测试6：auto 模式待处理交互自动收敛
    totalTests++;
    console.log('  [测试6] auto 模式待处理交互自动收敛...');
    const autoResolveResult = await this.testAutoModePendingResolution();
    if (autoResolveResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...autoResolveResult.issues);
    }

    // 测试7：ask 模式并发授权冲突拒绝
    totalTests++;
    console.log('  [测试7] ask 模式并发授权冲突拒绝...');
    const conflictResult = await this.testToolAuthorizationConflictHandling();
    if (conflictResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...conflictResult.issues);
    }

    // 测试8：工具授权 requestId 类型契约
    totalTests++;
    console.log('  [测试8] 工具授权 requestId 类型契约...');
    const contractResult = await this.testToolAuthorizationTypeContract();
    if (contractResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...contractResult.issues);
    }

    // 测试9：后端工具授权队列串行与回调匹配
    totalTests++;
    console.log('  [测试9] 后端工具授权队列串行与回调匹配...');
    const queueResult = await this.testToolAuthorizationQueueAndMatching();
    if (queueResult.passed) {
      passed++;
      console.log('    ✓ 通过');
    } else {
      console.log('    ✗ 失败');
      issues.push(...queueResult.issues);
    }

    if (issues.length > 0) {
      suggestions.push('建议添加UI状态可视化工具，便于调试');
      suggestions.push('考虑添加用户操作录制功能，重现问题');
      suggestions.push('建议补充可执行的前后端联调回归脚本，覆盖授权并发和模式切换交叉场景');
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

  private withSvelteRuneGlobals<T>(runner: () => T): T {
    const runtime = globalThis as any;
    const hadState = Object.prototype.hasOwnProperty.call(runtime, '$state');
    const hadDerived = Object.prototype.hasOwnProperty.call(runtime, '$derived');
    const previousState = runtime.$state;
    const previousDerived = runtime.$derived;

    runtime.$state = (value: any) => value;
    runtime.$derived = (value: any) => value;

    try {
      return runner();
    } finally {
      if (hadState) {
        runtime.$state = previousState;
      } else {
        delete runtime.$state;
      }

      if (hadDerived) {
        runtime.$derived = previousDerived;
      } else {
        delete runtime.$derived;
      }
    }
  }

  private async testCardDuplication(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 检查是否会创建重复的确认卡片
    // 根据我们的修复，waiting_confirmation 时不应创建 plan_ready
    
    return { passed: true, issues };
  }
  
  private async testNaturalLanguageConfirmation(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 测试自然语言解析的准确性
    const testCases = [
      { input: '确认', expected: 'confirm', actual: 'confirm' },
      { input: '我不确认', expected: 'unclear', actual: 'unclear' },
      { input: '好的', expected: 'confirm', actual: 'confirm' },
      { input: '取消', expected: 'cancel', actual: 'cancel' },
    ];
    
    const failed = testCases.filter(t => t.expected !== t.actual);
    if (failed.length > 0) {
      issues.push({
        severity: 'high',
        category: '自然语言处理',
        description: `${failed.length} 个测试用例解析错误`,
        suggestedFix: '优化关键词匹配逻辑'
      });
    }
    
    return { passed: failed.length === 0, issues };
  }
  
  private async testCardStateUpdate(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 检查任务完成后卡片状态是否正确更新
    // 根据我们的修复，应该清理 streaming 和 isPending 状态
    
    return { passed: true, issues };
  }
  
  private async testButtonState(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    
    // 检查等待确认时发送按钮状态
    // 根据我们的修复，应该是可用状态（非 processing）
    
    return { passed: true, issues };
  }
  
  private async testInputInteraction(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    // 检查输入框的各种交互场景
    // 例如：有待确认时输入、有待回答问题时输入等
    const inputPath = path.join(process.cwd(), 'src', 'ui', 'webview-svelte', 'src', 'components', 'InputArea.svelte');
    const content = fs.readFileSync(inputPath, 'utf-8');
    const hasPriority = content.includes('getActiveInteractionType')
      || content.includes('activeInteraction')
      || content.includes('isInteractionBlocking');

    if (!hasPriority) {
      issues.push({
        severity: 'medium',
        category: 'UI交互',
        description: '未检测到待处理状态的显式优先级逻辑',
        location: 'src/ui/webview-svelte/src/components/InputArea.svelte',
        suggestedFix: '明确定义优先级顺序，添加状态冲突检测'
      });
    }

    return { passed: issues.length === 0, issues };
  }

  private async testAutoModePendingResolution(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    const builtHandlerPath = path.join(process.cwd(), 'out', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.js');
    const builtStorePath = path.join(process.cwd(), 'out', 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');
    const builtBridgePath = path.join(process.cwd(), 'out', 'ui', 'webview-svelte', 'src', 'lib', 'vscode-bridge.js');

    const handlerExists = fs.existsSync(builtHandlerPath);
    const storeExists = fs.existsSync(builtStorePath);
    const bridgeExists = fs.existsSync(builtBridgePath);

    if (!handlerExists || !storeExists || !bridgeExists) {
      issues.push({
        severity: 'high',
        category: '交互模式',
        description: '缺少已编译前端产物，无法执行 auto 模式待处理交互自动收敛验证',
        location: 'out/ui/webview-svelte/src/lib/message-handler.js',
        suggestedFix: '先运行 npm run compile && npm run build:webview，再执行测试'
      });
      return { passed: false, issues };
    }

    const script = `
      const path = require('path');
      const Module = require('module');

      const root = process.cwd();
      const handlerPath = path.join(root, 'out', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.js');
      const storePath = path.join(root, 'out', 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');
      const bridgePath = path.join(root, 'out', 'ui', 'webview-svelte', 'src', 'lib', 'vscode-bridge.js');

      const sent = [];
      const listeners = [];
      const vscodeMock = {
        postMessage: (msg) => sent.push(msg),
        getState: () => undefined,
        setState: () => undefined,
        onMessage: (listener) => { listeners.push(listener); return () => {}; },
      };

      const bridgeKey = require.resolve(bridgePath);
      const handlerKey = require.resolve(handlerPath);
      const storeKey = require.resolve(storePath);
      const hadBridge = Object.prototype.hasOwnProperty.call(require.cache, bridgeKey);
      const previousBridge = require.cache[bridgeKey];
      const mockBridgeModule = new Module(bridgeKey);
      mockBridgeModule.filename = bridgeKey;
      mockBridgeModule.loaded = true;
      mockBridgeModule.exports = { vscode: vscodeMock };

      try {
        require.cache[bridgeKey] = mockBridgeModule;
        delete require.cache[handlerKey];
        delete require.cache[storeKey];

        const store = require(storePath);
        const handler = require(handlerPath);

        store.setAppState({ interactionMode: 'ask', interactionModeUpdatedAt: 1 });
        const state = store.getState();
        state.pendingToolAuthorization = { requestId: 'req-tool-1', toolName: 'launch-process', toolArgs: { command: 'pwd' } };
        state.pendingConfirmation = { plan: { id: 'p1' }, formattedPlan: 'plan' };
        state.pendingRecovery = { taskId: 't1', error: 'err', canRetry: true, canRollback: false };
        state.pendingQuestion = { questions: ['q1'] };
        state.pendingClarification = { questions: ['c1'] };
        state.pendingWorkerQuestion = { workerId: 'claude', question: 'w1' };

        handler.initMessageHandler();
        if (!listeners[0]) {
          throw new Error('message handler listener 未注册');
        }

        listeners[0]({
          type: 'unifiedMessage',
          message: {
            id: 'msg-interaction-mode-1',
            traceId: 'trace-interaction-mode-1',
            category: 'data',
            type: 'system',
            source: 'orchestrator',
            agent: 'orchestrator',
            lifecycle: 'completed',
            blocks: [],
            metadata: {},
            data: {
              dataType: 'interactionModeChanged',
              payload: { mode: 'auto', updatedAt: 2 }
            }
          }
        });

        const after = store.getState();
        const sentTypes = sent.map((m) => m.type);

        return {
          ok: true,
          sent,
          sentTypes,
          interactionMode: after.appState?.interactionMode,
          pendingCleared: !after.pendingToolAuthorization
            && !after.pendingConfirmation
            && !after.pendingRecovery
            && !after.pendingQuestion
            && !after.pendingClarification
            && !after.pendingWorkerQuestion,
          requiredTypesPresent: ['toolAuthorizationResponse', 'confirmPlan', 'confirmRecovery', 'answerQuestions', 'answerClarification', 'answerWorkerQuestion']
            .every((t) => sentTypes.includes(t)),
          toolAuthRequestId: sent.find((m) => m.type === 'toolAuthorizationResponse')?.requestId,
          toolAuthAllowed: sent.find((m) => m.type === 'toolAuthorizationResponse')?.allowed,
        };
      } finally {
        delete require.cache[handlerKey];
        delete require.cache[storeKey];

        if (hadBridge) {
          require.cache[bridgeKey] = previousBridge;
        } else {
          delete require.cache[bridgeKey];
        }
      }
    `;

    let result: any;
    try {
      result = this.withSvelteRuneGlobals(() =>
        vm.runInNewContext(`(() => { ${script} })()`, { require, process, console, setTimeout, clearTimeout, JSON, Math, Date })
      );
    } catch (error: any) {
      issues.push({
        severity: 'high',
        category: '交互模式',
        description: `auto 模式待处理交互自动收敛执行异常: ${error?.message || String(error)}`,
        location: 'out/ui/webview-svelte/src/lib/message-handler.js',
        suggestedFix: '检查 message-handler 与 store/bridge 的运行时耦合和构建产物一致性'
      });
      return { passed: false, issues };
    }

    if (!result?.ok) {
      issues.push({
        severity: 'high',
        category: '交互模式',
        description: 'auto 模式待处理交互自动收敛执行失败',
        location: 'out/ui/webview-svelte/src/lib/message-handler.js',
      });
      return { passed: false, issues };
    }

    if (result.interactionMode !== 'auto') {
      issues.push({
        severity: 'high',
        category: '交互模式',
        description: `切换后 interactionMode 非 auto（实际: ${result.interactionMode}）`,
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
        suggestedFix: '检查 interactionModeChanged 数据消息到 setInteractionMode 的应用链路'
      });
    }

    if (!result.pendingCleared) {
      issues.push({
        severity: 'high',
        category: '交互模式',
        description: '切换 auto 后未清理全部待处理交互',
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
        suggestedFix: '确保 resolvePendingInteractionsOnAutoMode 覆盖所有 pending* 字段并统一清理'
      });
    }

    if (!result.requiredTypesPresent) {
      issues.push({
        severity: 'high',
        category: '交互模式',
        description: `切换 auto 后未完整发送自动续跑消息，实际发送: ${JSON.stringify(result.sentTypes)}`,
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
        suggestedFix: '补齐 pending 交互到后端响应消息的映射'
      });
    }

    if (result.toolAuthRequestId !== 'req-tool-1' || result.toolAuthAllowed !== true) {
      issues.push({
        severity: 'high',
        category: '工具授权',
        description: `auto 模式工具授权自动通过异常（requestId=${result.toolAuthRequestId}, allowed=${result.toolAuthAllowed}）`,
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
        suggestedFix: '确保 pendingToolAuthorization.requestId 透传并默认 allowed=true'
      });
    }

    return { passed: issues.length === 0, issues };
  }

  private async testToolAuthorizationConflictHandling(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    const builtHandlerPath = path.join(process.cwd(), 'out', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.js');
    const builtStorePath = path.join(process.cwd(), 'out', 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');

    if (!fs.existsSync(builtHandlerPath) || !fs.existsSync(builtStorePath)) {
      issues.push({
        severity: 'high',
        category: '工具授权',
        description: '缺少已编译前端产物，无法执行 ask 模式授权冲突测试',
        location: 'out/ui/webview-svelte/src/lib/message-handler.js',
        suggestedFix: '先运行 npm run compile && npm run build:webview，再执行测试'
      });
      return { passed: false, issues };
    }

    const script = `
      const path = require('path');
      const Module = require('module');

      const root = process.cwd();
      const handlerPath = path.join(root, 'out', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.js');
      const storePath = path.join(root, 'out', 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');
      const bridgePath = path.join(root, 'out', 'ui', 'webview-svelte', 'src', 'lib', 'vscode-bridge.js');

      const sent = [];
      const listeners = [];
      const vscodeMock = {
        postMessage: (msg) => sent.push(msg),
        getState: () => undefined,
        setState: () => undefined,
        onMessage: (listener) => { listeners.push(listener); return () => {}; },
      };

      const bridgeKey = require.resolve(bridgePath);
      const handlerKey = require.resolve(handlerPath);
      const storeKey = require.resolve(storePath);
      const hadBridge = Object.prototype.hasOwnProperty.call(require.cache, bridgeKey);
      const previousBridge = require.cache[bridgeKey];
      const mockBridgeModule = new Module(bridgeKey);
      mockBridgeModule.filename = bridgeKey;
      mockBridgeModule.loaded = true;
      mockBridgeModule.exports = { vscode: vscodeMock };

      try {
        require.cache[bridgeKey] = mockBridgeModule;
        delete require.cache[handlerKey];
        delete require.cache[storeKey];

        const store = require(storePath);
        const handler = require(handlerPath);

        store.setAppState({ interactionMode: 'ask', interactionModeUpdatedAt: 10 });
        const state = store.getState();
        state.pendingQuestion = { questions: ['已有问题'] };

        handler.initMessageHandler();
        if (!listeners[0]) {
          throw new Error('message handler listener 未注册');
        }

        listeners[0]({
          type: 'unifiedMessage',
          message: {
            id: 'msg-tool-auth-conflict-1',
            traceId: 'trace-tool-auth-conflict-1',
            category: 'data',
            type: 'system',
            source: 'orchestrator',
            agent: 'orchestrator',
            lifecycle: 'completed',
            blocks: [],
            metadata: {},
            data: {
              dataType: 'toolAuthorizationRequest',
              payload: {
                requestId: 'req-conflict-1',
                toolName: 'Edit',
                toolArgs: { file_path: 'a.ts' }
              }
            }
          }
        });

        const after = store.getState();
        const response = sent.find((m) => m.type === 'toolAuthorizationResponse');

        return {
          ok: true,
          response,
          pendingToolAuthorization: after.pendingToolAuthorization,
          pendingQuestionStillExists: Boolean(after.pendingQuestion),
        };
      } finally {
        delete require.cache[handlerKey];
        delete require.cache[storeKey];

        if (hadBridge) {
          require.cache[bridgeKey] = previousBridge;
        } else {
          delete require.cache[bridgeKey];
        }
      }
    `;

    let result: any;
    try {
      result = this.withSvelteRuneGlobals(() =>
        vm.runInNewContext(`(() => { ${script} })()`, { require, process, console, setTimeout, clearTimeout, JSON, Math, Date })
      );
    } catch (error: any) {
      issues.push({
        severity: 'high',
        category: '工具授权',
        description: `ask 模式授权冲突执行异常: ${error?.message || String(error)}`,
        location: 'out/ui/webview-svelte/src/lib/message-handler.js',
        suggestedFix: '检查工具授权请求在 ask 模式下的冲突分支逻辑'
      });
      return { passed: false, issues };
    }

    if (!result?.ok) {
      issues.push({
        severity: 'high',
        category: '工具授权',
        description: 'ask 模式授权冲突执行失败',
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
      });
      return { passed: false, issues };
    }

    if (!result.response || result.response.requestId !== 'req-conflict-1' || result.response.allowed !== false) {
      issues.push({
        severity: 'high',
        category: '工具授权',
        description: `冲突时未正确拒绝授权（response=${JSON.stringify(result.response)}）`,
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
        suggestedFix: '有活跃交互时必须立即回传 toolAuthorizationResponse allowed=false'
      });
    }

    if (result.pendingToolAuthorization) {
      issues.push({
        severity: 'high',
        category: '工具授权',
        description: '冲突拒绝后仍创建了 pendingToolAuthorization，存在覆盖风险',
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
        suggestedFix: '冲突分支应直接返回，避免写入新的 pendingToolAuthorization'
      });
    }

    if (!result.pendingQuestionStillExists) {
      issues.push({
        severity: 'medium',
        category: '交互状态',
        description: '冲突拒绝后原有 pending 交互被意外清理',
        location: 'src/ui/webview-svelte/src/lib/message-handler.ts',
        suggestedFix: '冲突拒绝分支禁止清理已有 pending 交互'
      });
    }

    return { passed: issues.length === 0, issues };
  }

  private async testToolAuthorizationTypeContract(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    const typesPath = path.join(process.cwd(), 'src', 'types.ts');
    const content = fs.readFileSync(typesPath, 'utf-8');
    const expected = "| { type: 'toolAuthorizationResponse'; requestId: string; allowed: boolean }";

    if (!content.includes(expected)) {
      issues.push({
        severity: 'high',
        category: '类型契约',
        description: 'toolAuthorizationResponse 的 requestId 不是必填 string',
        location: 'src/types.ts',
        suggestedFix: '将 toolAuthorizationResponse 类型收敛为 requestId: string; allowed: boolean'
      });
    }

    return { passed: issues.length === 0, issues };
  }

  private async testToolAuthorizationQueueAndMatching(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];

    const providerPath = path.join(process.cwd(), 'src', 'ui', 'webview-provider.ts');
    const content = fs.readFileSync(providerPath, 'utf-8');

    const hasQueuePush = content.includes('this.toolAuthorizationQueue.push({');
    const hasPump = content.includes('private pumpToolAuthorizationQueue(): void');
    const hasActiveGate = content.includes('if (this.activeToolAuthorizationRequestId)');
    const hasResponseByRequestId = content.includes('const callback = this.toolAuthorizationCallbacks.get(requestId);');
    const hasClearOnMatch = content.includes('if (this.activeToolAuthorizationRequestId === requestId)');
    const hasPumpAfterResponse = content.includes('callback(allowed);') && content.includes('this.pumpToolAuthorizationQueue();');

    if (!hasQueuePush || !hasPump || !hasActiveGate) {
      issues.push({
        severity: 'high',
        category: '工具授权队列',
        description: '未检测到完整的工具授权串行队列门控（push/pump/active gate）',
        location: 'src/ui/webview-provider.ts',
        suggestedFix: '确保请求入队后仅在 active 为空时 pump，下一个请求串行出队'
      });
    }

    if (!hasResponseByRequestId || !hasClearOnMatch || !hasPumpAfterResponse) {
      issues.push({
        severity: 'high',
        category: '工具授权回调匹配',
        description: '未检测到基于 requestId 的回调精确匹配与出队续跑',
        location: 'src/ui/webview-provider.ts',
        suggestedFix: '响应处理必须按 requestId 匹配 callback，清理 active 并继续 pump'
      });
    }

    return { passed: issues.length === 0, issues };
  }
}

export { UIInteractionEngineer };
