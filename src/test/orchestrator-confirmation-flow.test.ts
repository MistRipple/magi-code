/**
 * 编排流程确认机制测试
 * 
 * 测试目标：
 * 1. 验证确认流程的状态管理
 * 2. 验证 plan_ready 和 plan_confirmation 不会重复
 * 3. 验证等待确认时的状态正确性
 */

import { EventEmitter } from 'events';

// ============================================================================
// 测试辅助类
// ============================================================================

/** 模拟编排者状态 */
class MockOrchestrator extends EventEmitter {
  private _state: string = 'idle';
  private confirmationCallback: ((plan: any, formatted: string) => Promise<boolean>) | null = null;
  
  get state(): string {
    return this._state;
  }
  
  setState(newState: string): void {
    const oldState = this._state;
    this._state = newState;
    this.emit('stateChange', newState);
    console.log(`[MockOrchestrator] 状态变更: ${oldState} -> ${newState}`);
  }
  
  setConfirmationCallback(callback: (plan: any, formatted: string) => Promise<boolean>): void {
    this.confirmationCallback = callback;
  }
  
  async execute(userPrompt: string): Promise<string> {
    console.log(`\n[MockOrchestrator] 开始执行: ${userPrompt}`);
    
    // Phase 1: 分析
    this.setState('analyzing');
    await this.delay(100);
    const plan = { id: 'plan-1', subTasks: [{ id: 'task-1', description: '实现功能' }] };
    const formattedPlan = '执行计划：\n- 任务1：实现功能';
    
    // Phase 2: 等待确认
    this.setState('waiting_confirmation');
    console.log('[MockOrchestrator] 等待用户确认...');
    
    if (!this.confirmationCallback) {
      console.log('[MockOrchestrator] 未设置确认回调，自动确认');
      this.setState('dispatching');
      return '任务已完成（自动确认）';
    }
    
    const confirmed = await this.confirmationCallback(plan, formattedPlan);
    console.log(`[MockOrchestrator] 用户确认结果: ${confirmed ? 'Y' : 'N'}`);
    
    if (!confirmed) {
      this.setState('idle');
      return '任务已取消';
    }
    
    // Phase 3: 执行
    this.setState('dispatching');
    await this.delay(100);
    this.setState('monitoring');
    await this.delay(100);
    
    // Phase 4: 完成
    this.setState('completed');
    return '任务已完成';
  }
  
  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}

/** 模拟前端状态管理 */
class MockFrontend {
  private messages: any[] = [];
  private isProcessing: boolean = false;
  private orchestratorPhase: string = 'idle';
  
  handleStateUpdate(state: { orchestratorPhase: string; activePlan?: any }): void {
    console.log(`[MockFrontend] 收到 stateUpdate: phase=${state.orchestratorPhase}`);
    this.orchestratorPhase = state.orchestratorPhase;
    
    if (state.activePlan) {
      const hasPlanConfirmation = this.messages.some(m => m.type === 'plan_confirmation' && m.isPending);
      const hasPlanPreview = this.messages.some(m => m.type === 'plan_ready');
      const isWaitingConfirmation = state.orchestratorPhase === 'waiting_confirmation';
      
      console.log(`[MockFrontend] activePlan 检查:`, {
        hasPlanConfirmation,
        hasPlanPreview,
        isWaitingConfirmation
      });
      
      if (!hasPlanPreview && !hasPlanConfirmation && !isWaitingConfirmation) {
        console.log('[MockFrontend] 创建 plan_ready 卡片');
        this.messages.push({ type: 'plan_ready', content: state.activePlan.formattedPlan });
      }
    }
  }
  
  handleConfirmationRequest(plan: any, formattedPlan: string): void {
    console.log('[MockFrontend] 收到 confirmationRequest');
    
    const planReadyIdx = this.messages.findIndex(m => m.type === 'plan_ready');
    if (planReadyIdx !== -1) {
      console.log('[MockFrontend] 找到 plan_ready，转换为 plan_confirmation');
      this.messages[planReadyIdx].type = 'plan_confirmation';
      this.messages[planReadyIdx].isPending = true;
    } else {
      console.log('[MockFrontend] 创建新的 plan_confirmation 卡片');
      this.messages.push({ type: 'plan_confirmation', content: formattedPlan, isPending: true });
    }
    
    // 关键：等待确认时应该停止处理状态
    this.setProcessingState(false);
  }
  
  setProcessingState(processing: boolean): void {
    const changed = this.isProcessing !== processing;
    this.isProcessing = processing;
    if (changed) {
      console.log(`[MockFrontend] 处理状态变更: ${processing ? '执行中' : '空闲'}`);
    }
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
    this.orchestratorPhase = 'idle';
  }
}

// ============================================================================
// 测试用例
// ============================================================================

/** 测试结果 */
interface TestResult {
  name: string;
  passed: boolean;
  errors: string[];
  duration: number;
}

/** 测试套件 */
class ConfirmationFlowTestSuite {
  private results: TestResult[] = [];

  async runAll(): Promise<void> {
    console.log('\n========================================');
    console.log('编排流程确认机制测试');
    console.log('========================================\n');

    await this.test1_BasicConfirmationFlow();
    await this.test2_NoDuplicateCards();
    await this.test3_ProcessingStateManagement();
    await this.test4_UserCancellation();

    this.printSummary();
  }

  /** 测试1：基本确认流程 */
  private async test1_BasicConfirmationFlow(): Promise<void> {
    const testName = '测试1：基本确认流程';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();

    try {
      const orchestrator = new MockOrchestrator();
      const frontend = new MockFrontend();
      let confirmationReceived = false;

      // 设置确认回调
      orchestrator.setConfirmationCallback(async (plan, formatted) => {
        confirmationReceived = true;
        console.log('[Test] 确认回调被调用');

        // 模拟前端处理
        frontend.handleConfirmationRequest(plan, formatted);

        // 模拟用户确认
        await new Promise(resolve => setTimeout(resolve, 50));
        return true;
      });

      // 监听状态变化
      const states: string[] = [];
      orchestrator.on('stateChange', (state: string) => {
        states.push(state);
      });

      // 执行任务
      const result = await orchestrator.execute('实现一个功能');

      // 验证
      if (!confirmationReceived) {
        errors.push('确认回调未被调用');
      }

      if (!states.includes('waiting_confirmation')) {
        errors.push('未进入 waiting_confirmation 状态');
      }

      if (!states.includes('dispatching')) {
        errors.push('确认后未进入 dispatching 状态');
      }

      const messages = frontend.getMessages();
      const hasConfirmation = messages.some(m => m.type === 'plan_confirmation');
      if (!hasConfirmation) {
        errors.push('前端未创建 plan_confirmation 卡片');
      }

      console.log(`✓ 状态序列: ${states.join(' -> ')}`);
      console.log(`✓ 前端消息数: ${messages.length}`);

    } catch (error) {
      errors.push(`异常: ${error instanceof Error ? error.message : String(error)}`);
    }

    this.recordResult(testName, errors, Date.now() - startTime);
  }

  /** 测试2：不会创建重复卡片 */
  private async test2_NoDuplicateCards(): Promise<void> {
    const testName = '测试2：不会创建重复卡片';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();

    try {
      const orchestrator = new MockOrchestrator();
      const frontend = new MockFrontend();

      orchestrator.setConfirmationCallback(async (plan, formatted) => {
        // 模拟 stateUpdate 先到达
        frontend.handleStateUpdate({
          orchestratorPhase: 'waiting_confirmation',
          activePlan: { formattedPlan: formatted }
        });

        // 然后 confirmationRequest 到达
        await new Promise(resolve => setTimeout(resolve, 10));
        frontend.handleConfirmationRequest(plan, formatted);

        return true;
      });

      await orchestrator.execute('实现功能');

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

  /** 测试3：处理状态管理 */
  private async test3_ProcessingStateManagement(): Promise<void> {
    const testName = '测试3：处理状态管理';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();

    try {
      const orchestrator = new MockOrchestrator();
      const frontend = new MockFrontend();

      frontend.setProcessingState(true); // 初始为执行中

      orchestrator.setConfirmationCallback(async (plan, formatted) => {
        frontend.handleConfirmationRequest(plan, formatted);

        // 验证：等待确认时应该停止处理状态
        const isProcessing = frontend.getProcessingState();
        if (isProcessing) {
          errors.push('等待确认时 isProcessing 应该为 false');
        } else {
          console.log('✓ 等待确认时 isProcessing = false');
        }

        return true;
      });

      await orchestrator.execute('实现功能');

    } catch (error) {
      errors.push(`异常: ${error instanceof Error ? error.message : String(error)}`);
    }

    this.recordResult(testName, errors, Date.now() - startTime);
  }

  /** 测试4：用户取消 */
  private async test4_UserCancellation(): Promise<void> {
    const testName = '测试4：用户取消';
    console.log(`\n>>> ${testName}`);
    const errors: string[] = [];
    const startTime = Date.now();

    try {
      const orchestrator = new MockOrchestrator();
      const states: string[] = [];

      orchestrator.on('stateChange', (state: string) => {
        states.push(state);
      });

      orchestrator.setConfirmationCallback(async () => {
        console.log('[Test] 用户取消确认');
        return false; // 用户取消
      });

      const result = await orchestrator.execute('实现功能');

      if (!result.includes('已取消')) {
        errors.push('取消后应返回取消消息');
      }

      if (states.includes('dispatching')) {
        errors.push('取消后不应进入 dispatching 状态');
      }

      if (!states.includes('idle')) {
        errors.push('取消后应返回 idle 状态');
      }

      console.log(`✓ 最终状态: ${states[states.length - 1]}`);

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
  const suite = new ConfirmationFlowTestSuite();
  await suite.runAll();
}

// 如果直接运行此文件
if (require.main === module) {
  main().catch(console.error);
}

export { ConfirmationFlowTestSuite, MockOrchestrator, MockFrontend };

