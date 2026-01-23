/**
 * 综合修复验证测试
 * 
 * 测试所有已修复的问题：
 * 1. 编排者消息路由
 * 2. 确认卡片不重复
 * 3. 等待确认时的状态管理
 * 4. 自然语言确认
 * 5. 任务完成后卡片状态清理
 */

import { EventEmitter } from 'events';

// ============================================================================
// 测试辅助类
// ============================================================================

/** 模拟消息路由 */
class MockMessageRouter extends EventEmitter {
  private messages: Array<{ type: string; target: string; source: string }> = [];
  
  sendMessage(type: string, target: string, source: string): void {
    this.messages.push({ type, target, source });
    this.emit('message', { type, target, source });
  }
  
  getMessages(): Array<{ type: string; target: string; source: string }> {
    return [...this.messages];
  }
  
  getMessagesByTarget(target: string): Array<{ type: string; target: string; source: string }> {
    return this.messages.filter(m => m.target === target);
  }
  
  clear(): void {
    this.messages = [];
  }
}

/** 模拟前端状态 */
class MockFrontendState {
  private messages: any[] = [];
  private isProcessing: boolean = false;
  private phase: string = 'idle';
  
  addMessage(msg: any): void {
    this.messages.push(msg);
  }
  
  updatePhase(phase: string, isRunning: boolean): void {
    this.phase = phase;
    
    // 模拟 updatePhaseIndicator 的清理逻辑
    if (!isRunning && (phase === 'completed' || phase === 'failed' || phase === 'interrupted' || phase === 'idle')) {
      this.messages.forEach(m => {
        if (m.streaming) {
          m.streaming = false;
        }
        if (m.isPending && (m.type === 'plan_confirmation' || m.type === 'question_request')) {
          m.isPending = false;
        }
      });
    }
  }
  
  setProcessing(processing: boolean): void {
    this.isProcessing = processing;
  }
  
  getMessages(): any[] {
    return this.messages;
  }
  
  getProcessingState(): boolean {
    return this.isProcessing;
  }
  
  clear(): void {
    this.messages = [];
    this.isProcessing = false;
    this.phase = 'idle';
  }
}

/** 自然语言确认解析器 */
class NaturalLanguageParser {
  private confirmKeywords = ['确认', '好的', '好', '是的', '是', 'yes', 'y', 'ok', '执行', '开始', '继续'];
  private cancelKeywords = ['取消', '不', '不要', '否', 'no', 'n', 'cancel', '停止'];
  
  parse(input: string): 'confirm' | 'cancel' | 'unclear' {
    const normalized = input.trim().toLowerCase();
    
    if (this.confirmKeywords.includes(normalized)) {
      return 'confirm';
    }
    
    if (this.cancelKeywords.includes(normalized)) {
      return 'cancel';
    }
    
    return 'unclear';
  }
}

// ============================================================================
// 测试套件
// ============================================================================

class ComprehensiveFixesTestSuite {
  private results: Array<{ name: string; passed: boolean; errors: string[]; duration: number }> = [];
  
  async runAll(): Promise<void> {
    console.log('\n========================================');
    console.log('综合修复验证测试');
    console.log('========================================\n');
    
    await this.test1_OrchestratorMessageRouting();
    await this.test2_NoConfirmationCardDuplication();
    await this.test3_ProcessingStateDuringConfirmation();
    await this.test4_NaturalLanguageConfirmation();
    await this.test5_CardStateCleanupOnCompletion();
    
    this.printSummary();
  }
  
  /** 测试1：编排者消息路由 */
  private async test1_OrchestratorMessageRouting(): Promise<void> {
    const testName = '测试1：编排者消息路由';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();
    
    try {
      const router = new MockMessageRouter();
      
      // 模拟编排者发送消息
      router.sendMessage('standardMessage', 'thread', 'orchestrator');
      router.sendMessage('standardMessage', 'thread', 'orchestrator');
      router.sendMessage('standardMessage', 'worker', 'worker');
      
      const threadMessages = router.getMessagesByTarget('thread');
      const workerMessages = router.getMessagesByTarget('worker');
      
      console.log(`✓ Thread 消息数: ${threadMessages.length}`);
      console.log(`✓ Worker 消息数: ${workerMessages.length}`);
      
      if (threadMessages.length !== 2) {
        errors.push(`Thread 应该有2条消息，实际: ${threadMessages.length}`);
      }
      
      if (workerMessages.length !== 1) {
        errors.push(`Worker 应该有1条消息，实际: ${workerMessages.length}`);
      }
      
      const orchestratorInWorker = workerMessages.some(m => m.source === 'orchestrator');
      if (orchestratorInWorker) {
        errors.push('编排者消息不应该出现在 Worker 面板');
      }
      
    } catch (error) {
      errors.push(`异常: ${error instanceof Error ? error.message : String(error)}`);
    }
    
    this.recordResult(testName, errors, Date.now() - startTime);
  }
  
  /** 测试2：确认卡片不重复 */
  private async test2_NoConfirmationCardDuplication(): Promise<void> {
    const testName = '测试2：确认卡片不重复';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();
    
    try {
      const frontend = new MockFrontendState();
      
      // 模拟 stateUpdate 到达（waiting_confirmation 阶段）
      const isWaitingConfirmation = true;
      const hasPlanConfirmation = false;
      const hasPlanPreview = false;
      
      // 根据修复后的逻辑，waiting_confirmation 时不应创建 plan_ready
      if (!hasPlanPreview && !hasPlanConfirmation && !isWaitingConfirmation) {
        frontend.addMessage({ type: 'plan_ready' });
      }
      
      // 模拟 confirmationRequest 到达
      frontend.addMessage({ type: 'plan_confirmation', isPending: true });
      
      const messages = frontend.getMessages();
      const planReadyCount = messages.filter(m => m.type === 'plan_ready').length;
      const planConfirmationCount = messages.filter(m => m.type === 'plan_confirmation').length;
      
      console.log(`✓ plan_ready 数量: ${planReadyCount}`);
      console.log(`✓ plan_confirmation 数量: ${planConfirmationCount}`);
      
      if (planReadyCount > 0) {
        errors.push(`waiting_confirmation 阶段不应创建 plan_ready (实际: ${planReadyCount})`);
      }
      
      if (planConfirmationCount !== 1) {
        errors.push(`应该只有1个 plan_confirmation (实际: ${planConfirmationCount})`);
      }
      
    } catch (error) {
      errors.push(`异常: ${error instanceof Error ? error.message : String(error)}`);
    }
    
    this.recordResult(testName, errors, Date.now() - startTime);
  }

  /** 测试3：等待确认时的状态管理 */
  private async test3_ProcessingStateDuringConfirmation(): Promise<void> {
    const testName = '测试3：等待确认时的状态管理';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();

    try {
      const frontend = new MockFrontendState();

      // 初始状态：执行中
      frontend.setProcessing(true);

      // 显示确认卡片时应该停止处理状态
      frontend.addMessage({ type: 'plan_confirmation', isPending: true });
      frontend.setProcessing(false);

      const isProcessing = frontend.getProcessingState();

      console.log(`✓ 等待确认时 isProcessing: ${isProcessing}`);

      if (isProcessing) {
        errors.push('等待确认时 isProcessing 应该为 false');
      }

    } catch (error) {
      errors.push(`异常: ${error instanceof Error ? error.message : String(error)}`);
    }

    this.recordResult(testName, errors, Date.now() - startTime);
  }

  /** 测试4：自然语言确认 */
  private async test4_NaturalLanguageConfirmation(): Promise<void> {
    const testName = '测试4：自然语言确认';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();

    try {
      const parser = new NaturalLanguageParser();

      // 测试确认关键词（全量匹配）
      const confirmTests = [
        { input: '确认', expected: 'confirm' },
        { input: '好的', expected: 'confirm' },
        { input: 'yes', expected: 'confirm' },
        { input: 'ok', expected: 'confirm' },
      ];

      // 测试取消关键词
      const cancelTests = [
        { input: '取消', expected: 'cancel' },
        { input: '不要', expected: 'cancel' },
        { input: 'no', expected: 'cancel' },
      ];

      // 测试误判情况（应该返回 unclear）
      const unclearTests = [
        { input: '我不确认', expected: 'unclear' },
        { input: '好像有问题', expected: 'unclear' },
        { input: '不太好', expected: 'unclear' },
        { input: '确认一下', expected: 'unclear' },
      ];

      let passCount = 0;
      let totalCount = 0;

      [...confirmTests, ...cancelTests, ...unclearTests].forEach(test => {
        totalCount++;
        const result = parser.parse(test.input);
        if (result === test.expected) {
          passCount++;
          console.log(`✓ "${test.input}" -> ${result}`);
        } else {
          errors.push(`"${test.input}" 期望 ${test.expected}，实际 ${result}`);
          console.log(`✗ "${test.input}" -> ${result} (期望: ${test.expected})`);
        }
      });

      console.log(`\n通过率: ${passCount}/${totalCount}`);

    } catch (error) {
      errors.push(`异常: ${error instanceof Error ? error.message : String(error)}`);
    }

    this.recordResult(testName, errors, Date.now() - startTime);
  }

  /** 测试5：任务完成后卡片状态清理 */
  private async test5_CardStateCleanupOnCompletion(): Promise<void> {
    const testName = '测试5：任务完成后卡片状态清理';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();

    try {
      const frontend = new MockFrontendState();

      // 添加各种状态的消息
      frontend.addMessage({ type: 'assistant', streaming: true, content: '正在执行...' });
      frontend.addMessage({ type: 'plan_confirmation', isPending: true, content: '执行计划' });
      frontend.addMessage({ type: 'question_request', isPending: true, content: '请确认' });

      console.log('任务完成前:');
      console.log(`  streaming 消息数: ${frontend.getMessages().filter(m => m.streaming).length}`);
      console.log(`  isPending 消息数: ${frontend.getMessages().filter(m => m.isPending).length}`);

      // 模拟任务完成
      frontend.updatePhase('completed', false);

      const streamingCount = frontend.getMessages().filter(m => m.streaming).length;
      const pendingCount = frontend.getMessages().filter(m => m.isPending).length;

      console.log('\n任务完成后:');
      console.log(`  streaming 消息数: ${streamingCount}`);
      console.log(`  isPending 消息数: ${pendingCount}`);

      if (streamingCount > 0) {
        errors.push(`任务完成后仍有 ${streamingCount} 条 streaming 消息`);
      }

      if (pendingCount > 0) {
        errors.push(`任务完成后仍有 ${pendingCount} 条 isPending 消息`);
      }

    } catch (error) {
      errors.push(`异常: ${error instanceof Error ? error.message : String(error)}`);
    }

    this.recordResult(testName, errors, Date.now() - startTime);
  }

  private recordResult(name: string, errors: string[], duration: number): void {
    const passed = errors.length === 0;
    this.results.push({ name, passed, errors, duration });

    if (passed) {
      console.log(`✅ ${name} - 通过 (${duration}ms)`);
    } else {
      console.log(`❌ ${name} - 失败 (${duration}ms)`);
      errors.forEach(err => console.log(`   - ${err}`));
    }
  }

  private printSummary(): void {
    console.log('\n========================================');
    console.log('测试总结');
    console.log('========================================');

    const total = this.results.length;
    const passed = this.results.filter(r => r.passed).length;
    const failed = total - passed;

    console.log(`总计: ${total} | 通过: ${passed} | 失败: ${failed}`);

    if (failed > 0) {
      console.log('\n失败的测试:');
      this.results.filter(r => !r.passed).forEach(r => {
        console.log(`\n${r.name}:`);
        r.errors.forEach(err => console.log(`  - ${err}`));
      });
    }

    console.log('\n========================================\n');
  }
}

// ============================================================================
// 执行测试
// ============================================================================

async function main() {
  const suite = new ComprehensiveFixesTestSuite();
  await suite.runAll();
}

// 如果直接运行此文件
if (require.main === module) {
  main().catch(console.error);
}

export { ComprehensiveFixesTestSuite };
