/**
 * 意图分类端对端测试
 *
 * 测试真实 LLM 对各种用户输入的意图分类结果
 * 验证 ASK/DIRECT/EXPLORE/TASK/CLARIFY 模式的正确触发
 */

import { TestEngineer, TestReport, TestIssue } from '../test-command-center';

interface IntentTestCase {
  name: string;
  input: string;
  expectedMode: 'ask' | 'direct' | 'explore' | 'task' | 'clarify';
  expectedIntent?: string;
  description: string;
}

class IntentClassificationE2EEngineer implements TestEngineer {
  name = '意图分类E2E测试专家';
  specialty = '测试真实LLM意图分类、各模式路由、CLARIFY静默跳过';

  // 测试用例
  private testCases: IntentTestCase[] = [
    // ASK 模式 - 简单问答
    {
      name: 'ASK-问候',
      input: '你好',
      expectedMode: 'ask',
      expectedIntent: 'question',
      description: '简单问候应进入 ASK 模式',
    },
    {
      name: 'ASK-技术问答',
      input: 'TypeScript 中 interface 和 type 有什么区别？',
      expectedMode: 'ask',
      expectedIntent: 'question',
      description: '技术概念问答应进入 ASK 模式',
    },
    {
      name: 'ASK-帮助请求',
      input: '你能做什么？',
      expectedMode: 'ask',
      expectedIntent: 'question',
      description: '能力询问应进入 ASK 模式',
    },

    // DIRECT 模式 - 简单代码操作（注意：不涉及代码文件的简单计算/格式化应归类为 ASK）
    {
      name: 'DIRECT-变量重命名',
      input: '把 getUserInfo 函数名改成 fetchUserProfile',
      expectedMode: 'direct',
      expectedIntent: 'trivial',
      description: '简单代码重命名应进入 DIRECT 模式',
    },
    {
      name: 'DIRECT-添加注释',
      input: '给这个函数加上 JSDoc 注释',
      expectedMode: 'direct',
      expectedIntent: 'trivial',
      description: '添加代码注释应进入 DIRECT 模式',
    },

    // EXPLORE 模式 - 代码探索
    {
      name: 'EXPLORE-代码理解',
      input: '解释一下 MessageHub 是怎么工作的',
      expectedMode: 'explore',
      expectedIntent: 'exploratory',
      description: '代码理解请求应进入 EXPLORE 模式',
    },
    {
      name: 'EXPLORE-架构分析',
      input: '分析一下这个项目的消息流架构',
      expectedMode: 'explore',
      expectedIntent: 'exploratory',
      description: '架构分析应进入 EXPLORE 模式',
    },

    // TASK 模式 - 需要执行的任务
    {
      name: 'TASK-多文件重构',
      input: '重构用户认证模块，将登录、注册、密码重置逻辑分离到独立文件',
      expectedMode: 'task',
      expectedIntent: 'task',
      description: '多文件重构应进入 TASK 模式',
    },
    {
      name: 'TASK-Bug修复',
      input: '修复 MessageItem 组件在流式消息时卡片高度塌陷的问题',
      expectedMode: 'task',
      expectedIntent: 'task',
      description: '修复 Bug 应进入 TASK 模式',
    },
    {
      name: 'TASK-功能添加',
      input: '给登录页面添加记住密码功能',
      expectedMode: 'task',
      expectedIntent: 'task',
      description: '添加功能应进入 TASK 模式',
    },

    // CLARIFY 模式 - 模糊请求（在 auto 模式下应静默跳过）
    {
      name: 'CLARIFY-模糊优化',
      input: '优化一下',
      expectedMode: 'clarify',
      expectedIntent: 'ambiguous',
      description: '极度模糊的请求应触发 CLARIFY（auto 模式下静默跳过）',
    },
    {
      name: 'CLARIFY-不明确改进',
      input: '改进性能',
      expectedMode: 'clarify',
      expectedIntent: 'ambiguous',
      description: '缺少具体目标的请求应触发 CLARIFY',
    },
  ];

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
    console.log('  意图分类端对端测试');
    console.log('  ═══════════════════════════════════════════════════════\n');

    // 检查是否有真实 LLM 配置
    const hasLLMConfig = await this.checkLLMConfig();
    if (!hasLLMConfig) {
      console.log('  ⚠️  未检测到有效的 LLM 配置，跳过真实 LLM 测试');
      console.log('  ⚠️  仅执行静态代码检查\n');

      // 执行静态检查
      totalTests++;
      const staticResult = await this.runStaticChecks();
      if (staticResult.passed) {
        passed++;
        console.log('  ✓ 静态代码检查通过');
      } else {
        issues.push(...staticResult.issues);
        console.log('  ✗ 静态代码检查失败');
      }
    } else {
      // 尝试执行第一个测试，检查 API 是否可用
      let llmAvailable = true;
      let firstTestError: string | null = null;

      try {
        console.log('  正在验证 LLM API 连接...');
        const firstTest = this.testCases[0];
        await this.runSingleTest(firstTest);
        console.log('  ✓ LLM API 连接正常\n');
      } catch (error: any) {
        const errMsg = error?.message || String(error);
        // 检查是否是认证错误或网络错误
        if (errMsg.includes('401') || errMsg.includes('authentication') || errMsg.includes('api-key') || errMsg.includes('ECONNREFUSED')) {
          llmAvailable = false;
          firstTestError = errMsg;
          console.log(`  ⚠️  LLM API 不可用: ${errMsg.substring(0, 80)}...`);
          console.log('  ⚠️  降级到静态代码检查模式\n');
        }
      }

      if (!llmAvailable) {
        // 降级到静态检查
        totalTests++;
        const staticResult = await this.runStaticChecks();
        if (staticResult.passed) {
          passed++;
          console.log('  ✓ 静态代码检查通过');
        } else {
          issues.push(...staticResult.issues);
          console.log('  ✗ 静态代码检查失败');
        }

        // 添加一个说明性的 issue
        issues.push({
          severity: 'low',
          category: 'LLM配置',
          description: `LLM API 不可用，已跳过真实意图分类测试: ${firstTestError?.substring(0, 60)}...`,
          suggestedFix: '配置有效的 API key 以运行完整的意图分类测试',
        });
      } else {
        // 执行真实 LLM 测试
        for (const testCase of this.testCases) {
          totalTests++;
          console.log(`  [${testCase.name}] ${testCase.description}`);
          console.log(`    输入: "${testCase.input.substring(0, 50)}${testCase.input.length > 50 ? '...' : ''}"`);

          try {
            const result = await this.runSingleTest(testCase);
            if (result.passed) {
              passed++;
              console.log(`    ✓ 通过 (mode: ${result.actualMode}, intent: ${result.actualIntent})`);
            } else {
              console.log(`    ✗ 失败`);
              console.log(`      期望: mode=${testCase.expectedMode}, intent=${testCase.expectedIntent}`);
              console.log(`      实际: mode=${result.actualMode}, intent=${result.actualIntent}`);
              issues.push({
                severity: 'medium',
                category: '意图分类',
                description: `${testCase.name}: 期望 ${testCase.expectedMode}/${testCase.expectedIntent}，实际 ${result.actualMode}/${result.actualIntent}`,
                suggestedFix: '检查意图分类 prompt 或 LLM 响应解析逻辑',
              });
            }
          } catch (error: any) {
            console.log(`    ✗ 异常: ${error?.message || String(error)}`);
            issues.push({
              severity: 'high',
              category: '意图分类',
              description: `${testCase.name}: 测试执行异常 - ${error?.message || String(error)}`,
              suggestedFix: '检查 LLM 连接或意图分类流程',
            });
          }
          console.log('');
        }
      }
    }

    // 额外测试：CLARIFY 模式在 auto 下不显示 toast
    totalTests++;
    console.log('  [CLARIFY-静默跳过] 验证 auto 模式下 CLARIFY 不显示 toast');
    const clarifyResult = await this.verifyClarifyAutoSkip();
    if (clarifyResult.passed) {
      passed++;
      console.log('    ✓ 代码正确实现 autoSkipped 标记');
    } else {
      console.log('    ✗ 代码未正确实现 autoSkipped 标记');
      issues.push(...clarifyResult.issues);
    }

    console.log('\n  ═══════════════════════════════════════════════════════\n');

    if (issues.length > 0) {
      suggestions.push('建议检查意图分类 prompt 的清晰度');
      suggestions.push('考虑添加更多测试用例覆盖边界情况');
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

  private async checkLLMConfig(): Promise<boolean> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadOrchestratorConfig();
      return !!(config && config.baseUrl && config.model);
    } catch {
      return false;
    }
  }

  private async runStaticChecks(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    const fs = await import('fs');
    const path = await import('path');

    // 检查意图分类 prompt
    const promptPath = path.join(process.cwd(), 'src', 'orchestrator', 'prompts', 'intent-classification.ts');
    if (!fs.existsSync(promptPath)) {
      issues.push({
        severity: 'high',
        category: '静态检查',
        description: '意图分类 prompt 文件不存在',
        suggestedFix: '创建 src/orchestrator/prompts/intent-classification.ts',
      });
    } else {
      const content = fs.readFileSync(promptPath, 'utf-8');
      const requiredModes = ['ask', 'direct', 'explore', 'task', 'clarify'];
      for (const mode of requiredModes) {
        if (!content.includes(mode)) {
          issues.push({
            severity: 'medium',
            category: '静态检查',
            description: `意图分类 prompt 中缺少 ${mode} 模式的说明`,
            suggestedFix: `在 prompt 中添加 ${mode} 模式的示例和说明`,
          });
        }
      }
    }

    // 检查 IntentGate 实现
    const intentGatePath = path.join(process.cwd(), 'src', 'orchestrator', 'intent-gate.ts');
    if (fs.existsSync(intentGatePath)) {
      const content = fs.readFileSync(intentGatePath, 'utf-8');
      if (!content.includes('IntentHandlerMode')) {
        issues.push({
          severity: 'high',
          category: '静态检查',
          description: 'IntentGate 缺少 IntentHandlerMode 枚举',
          suggestedFix: '确保 IntentHandlerMode 枚举正确定义',
        });
      }
    }

    // 检查 auto 模式下 CLARIFY 的处理
    const messageHandlerPath = path.join(process.cwd(), 'src', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.ts');
    if (fs.existsSync(messageHandlerPath)) {
      const content = fs.readFileSync(messageHandlerPath, 'utf-8');
      if (!content.includes('autoSkipped')) {
        issues.push({
          severity: 'medium',
          category: '静态检查',
          description: 'message-handler.ts 中缺少 autoSkipped 标记',
          suggestedFix: '在 auto 模式跳过澄清时添加 autoSkipped: true',
        });
      }
    }

    return { passed: issues.length === 0, issues };
  }

  private async runSingleTest(testCase: IntentTestCase): Promise<{
    passed: boolean;
    actualMode: string;
    actualIntent: string;
  }> {
    // 此方法需要真实 LLM 调用
    // 在没有 LLM 配置时会被跳过

    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const { buildIntentClassificationPrompt } = await import('../../orchestrator/prompts/intent-classification');

      const config = LLMConfigLoader.loadOrchestratorConfig();
      const client = createLLMClient(config);

      const prompt = buildIntentClassificationPrompt(testCase.input);
      const response = await client.sendMessage({
        messages: [{ role: 'user', content: prompt }],
        maxTokens: 500,
        temperature: 0.1,
      });

      const content = response.content || '';
      const jsonMatch = content.match(/\{[\s\S]*\}/);
      if (!jsonMatch) {
        return { passed: false, actualMode: 'parse_error', actualIntent: 'parse_error' };
      }

      const parsed = JSON.parse(jsonMatch[0]);
      const actualMode = parsed.recommendedMode || 'unknown';
      const actualIntent = parsed.intent || 'unknown';

      const modeMatch = actualMode === testCase.expectedMode;
      // 意图匹配可以更宽松（因为 LLM 可能有不同表述）
      const intentMatch = !testCase.expectedIntent || actualIntent === testCase.expectedIntent;

      return {
        passed: modeMatch && intentMatch,
        actualMode,
        actualIntent,
      };
    } catch (error: any) {
      throw error;
    }
  }

  private async verifyClarifyAutoSkip(): Promise<{ passed: boolean; issues: TestIssue[] }> {
    const issues: TestIssue[] = [];
    const fs = await import('fs');
    const path = await import('path');

    // 检查前端 message-handler.ts
    const handlerPath = path.join(process.cwd(), 'src', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.ts');
    const handlerContent = fs.readFileSync(handlerPath, 'utf-8');

    if (!handlerContent.includes("autoSkipped: true")) {
      issues.push({
        severity: 'high',
        category: 'CLARIFY处理',
        description: 'message-handler.ts 在 auto 模式跳过澄清时未设置 autoSkipped: true',
        suggestedFix: '在 handleClarificationRequest 的 auto 分支中添加 autoSkipped: true',
      });
    }

    // 检查后端 webview-provider.ts
    const providerPath = path.join(process.cwd(), 'src', 'ui', 'webview-provider.ts');
    const providerContent = fs.readFileSync(providerPath, 'utf-8');

    if (!providerContent.includes('autoSkipped')) {
      issues.push({
        severity: 'high',
        category: 'CLARIFY处理',
        description: 'webview-provider.ts 的 handleClarificationAnswer 未处理 autoSkipped 参数',
        suggestedFix: '在 handleClarificationAnswer 中添加 autoSkipped 参数并跳过 toast',
      });
    }

    if (!providerContent.includes('if (!autoSkipped)')) {
      issues.push({
        severity: 'medium',
        category: 'CLARIFY处理',
        description: 'webview-provider.ts 未根据 autoSkipped 条件跳过 toast',
        suggestedFix: '添加 if (!autoSkipped) { this.sendToast(...) } 逻辑',
      });
    }

    return { passed: issues.length === 0, issues };
  }
}

export { IntentClassificationE2EEngineer };
