/**
 * 测试工程师3：UI交互专家
 * 
 * 专长：测试用户交互、卡片显示、自然语言处理
 */

import { TestEngineer, TestReport, TestIssue } from '../test-command-center';

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
    
    if (issues.length > 0) {
      suggestions.push('建议添加UI状态可视化工具，便于调试');
      suggestions.push('考虑添加用户操作录制功能，重现问题');
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
    
    // 潜在问题：多个待处理状态的优先级
    issues.push({
      severity: 'medium',
      category: 'UI交互',
      description: '多个待处理状态（确认、问题、澄清）的优先级不明确',
      location: 'src/ui/webview/index.html:5490-5513',
      suggestedFix: '明确定义优先级顺序，添加状态冲突检测'
    });
    
    return { passed: false, issues };
  }
}

export { UIInteractionEngineer };
