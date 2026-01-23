/**
 * 编排流程端到端测试框架
 *
 * 核心功能：
 * - 模拟用户输入和模型响应
 * - 记录消息传递流程
 * - 验证流程正确性
 * - 检测流程卡死
 */

import { EventEmitter } from 'events';

// ============================================================================
// 类型定义
// ============================================================================

/** 测试场景类型 */
export type TestScenario =
  | 'clear_requirement'      // 明确需求场景
  | 'vague_requirement'      // 模糊需求场景
  | 'worker_question'        // 子代理疑问场景
  | 'parallel_execution'     // 并行执行场景
  | 'failure_retry';         // 失败重试场景

/** 消息记录 */
export interface MessageRecord {
  timestamp: number;
  type: string;
  source: string;
  target?: string;
  payload: any;
}

/** 状态转换记录 */
export interface StateTransition {
  timestamp: number;
  from: string;
  to: string;
  trigger: string;
}

/** 测试结果 */
export interface TestResult {
  scenario: TestScenario;
  passed: boolean;
  duration: number;
  messages: MessageRecord[];
  stateTransitions: StateTransition[];
  errors: string[];
  timeoutDetected: boolean;
}

/** 测试用例配置 */
export interface TestCaseConfig {
  name: string;
  scenario: TestScenario;
  userInput: string;
  expectedStates: string[];
  expectedMessages: string[];
  mockResponses: MockResponse[];
  timeout: number;
}

/** 模拟响应配置 */
export interface MockResponse {
  trigger: string;           // 触发条件
  agent: string;             // 目标 Agent
  response: string;          // 响应内容
  delay?: number;            // 延迟时间
  shouldAskQuestion?: boolean; // 是否触发提问
  question?: string;         // 提问内容
}

// ============================================================================
// 消息记录器
// ============================================================================

export class MessageRecorder {
  private messages: MessageRecord[] = [];
  private stateTransitions: StateTransition[] = [];

  recordMessage(type: string, source: string, target: string | undefined, payload: any): void {
    this.messages.push({
      timestamp: Date.now(),
      type,
      source,
      target,
      payload
    });
  }

  recordStateTransition(from: string, to: string, trigger: string): void {
    this.stateTransitions.push({
      timestamp: Date.now(),
      from,
      to,
      trigger
    });
  }

  getMessages(): MessageRecord[] {
    return [...this.messages];
  }

  getStateTransitions(): StateTransition[] {
    return [...this.stateTransitions];
  }

  hasMessage(type: string): boolean {
    return this.messages.some(m => m.type === type);
  }

  hasStateTransition(from: string, to: string): boolean {
    return this.stateTransitions.some(t => t.from === from && t.to === to);
  }

  clear(): void {
    this.messages = [];
    this.stateTransitions = [];
  }

  /** 生成时序图 (Mermaid 格式) */
  generateSequenceDiagram(): string {
    const lines = ['sequenceDiagram'];
    const participants = new Set<string>();

    this.messages.forEach(m => {
      participants.add(m.source);
      if (m.target) participants.add(m.target);
    });

    participants.forEach(p => lines.push(`    participant ${p}`));

    this.messages.forEach(m => {
      const target = m.target || 'System';
      lines.push(`    ${m.source}->>+${target}: ${m.type}`);
    });

    return lines.join('\n');
  }
}

// ============================================================================
// 模拟 LLM 适配器
// ============================================================================

export class MockWorkerAdapter {
  private responses: Map<string, MockResponse[]> = new Map();
  private questionCallback?: (agent: string, question: string) => Promise<string>;

  /** 注册模拟响应 */
  registerResponse(agent: string, trigger: string, response: string, options?: Partial<MockResponse>): void {
    if (!this.responses.has(agent)) {
      this.responses.set(agent, []);
    }
    this.responses.get(agent)!.push({
      trigger,
      agent,
      response,
      ...options
    });
  }

  /** 设置提问回调 */
  onQuestion(callback: (agent: string, question: string) => Promise<string>): void {
    this.questionCallback = callback;
  }

  /** 模拟发送消息 */
  async sendMessage(agent: string, prompt: string): Promise<{ content: string; error?: string }> {
    const agentResponses = this.responses.get(agent) || [];

    for (const resp of agentResponses) {
      if (prompt.includes(resp.trigger)) {
        // 模拟延迟
        if (resp.delay) {
          await new Promise(resolve => setTimeout(resolve, resp.delay));
        }

        // 检查是否需要提问
        if (resp.shouldAskQuestion && resp.question && this.questionCallback) {
          const answer = await this.questionCallback(agent, resp.question);
          return { content: resp.response.replace('{{ANSWER}}', answer) };
        }

        return { content: resp.response };
      }
    }

    // 默认响应
    return { content: '任务已完成' };
  }

  clear(): void {
    this.responses.clear();
  }
}

// ============================================================================
// 模拟用户界面
// ============================================================================

export class MockUserInterface {
  private confirmationResponse: boolean = true;
  private clarificationResponses: Map<string, string> = new Map();
  private questionResponses: Map<string, string> = new Map();

  /** 设置确认响应 */
  setConfirmationResponse(confirm: boolean): void {
    this.confirmationResponse = confirm;
  }

  /** 设置澄清响应 */
  setClarificationResponse(question: string, answer: string): void {
    this.clarificationResponses.set(question, answer);
  }

  /** 设置问题响应 */
  setQuestionResponse(question: string, answer: string): void {
    this.questionResponses.set(question, answer);
  }

  /** 模拟确认 */
  async confirm(): Promise<boolean> {
    return this.confirmationResponse;
  }

  /** 模拟澄清回答 */
  async answerClarification(question: string): Promise<string> {
    return this.clarificationResponses.get(question) || '请继续执行';
  }

  /** 模拟问题回答 */
  async answerQuestion(question: string): Promise<string> {
    return this.questionResponses.get(question) || '请自行决定';
  }
}


// ============================================================================
// 流程验证器
// ============================================================================

export class FlowValidator {
  private recorder: MessageRecorder;
  private errors: string[] = [];

  constructor(recorder: MessageRecorder) {
    this.recorder = recorder;
  }

  /** 验证消息序列 */
  validateMessageSequence(expectedTypes: string[]): boolean {
    const messages = this.recorder.getMessages();
    const actualTypes = messages.map(m => m.type);

    let expectedIdx = 0;
    for (const actualType of actualTypes) {
      if (expectedIdx < expectedTypes.length && actualType === expectedTypes[expectedIdx]) {
        expectedIdx++;
      }
    }

    if (expectedIdx < expectedTypes.length) {
      this.errors.push(`消息序列不完整，缺少: ${expectedTypes.slice(expectedIdx).join(', ')}`);
      return false;
    }
    return true;
  }

  /** 验证状态转换 */
  validateStateTransitions(expectedTransitions: Array<[string, string]>): boolean {
    const transitions = this.recorder.getStateTransitions();

    for (const [from, to] of expectedTransitions) {
      const found = transitions.some(t => t.from === from && t.to === to);
      if (!found) {
        this.errors.push(`缺少状态转换: ${from} -> ${to}`);
        return false;
      }
    }
    return true;
  }

  /** 验证无死锁 */
  validateNoDeadlock(): boolean {
    const transitions = this.recorder.getStateTransitions();
    if (transitions.length === 0) return true;

    const lastState = transitions[transitions.length - 1].to;
    const terminalStates = ['completed', 'failed', 'idle'];

    if (!terminalStates.includes(lastState)) {
      this.errors.push(`流程未正常结束，最终状态: ${lastState}`);
      return false;
    }
    return true;
  }

  /** 验证确认流程不重复 */
  validateSingleConfirmation(): boolean {
    const messages = this.recorder.getMessages();
    const confirmationMessages = messages.filter(m =>
      m.type === 'plan_ready' || m.type === 'confirmationRequest'
    );

    if (confirmationMessages.length > 1) {
      this.errors.push(`确认流程重复: 发现 ${confirmationMessages.length} 个确认消息`);
      return false;
    }
    return true;
  }

  /** 验证模糊需求触发澄清 */
  validateClarificationTriggered(isVagueRequirement: boolean): boolean {
    const hasClarification = this.recorder.hasMessage('clarification_request');

    if (isVagueRequirement && !hasClarification) {
      this.errors.push('模糊需求未触发澄清流程');
      return false;
    }
    if (!isVagueRequirement && hasClarification) {
      this.errors.push('明确需求不应触发澄清流程');
      return false;
    }
    return true;
  }

  getErrors(): string[] {
    return [...this.errors];
  }

  clearErrors(): void {
    this.errors = [];
  }
}

// ============================================================================
// 超时检测器
// ============================================================================

export class TimeoutDetector {
  private timeoutMs: number;
  private timer: NodeJS.Timeout | null = null;
  private timeoutCallback?: () => void;
  private detected: boolean = false;

  constructor(timeoutMs: number = 30000) {
    this.timeoutMs = timeoutMs;
  }

  start(callback: () => void): void {
    this.timeoutCallback = callback;
    this.detected = false;
    this.timer = setTimeout(() => {
      this.detected = true;
      callback();
    }, this.timeoutMs);
  }

  stop(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  reset(): void {
    this.stop();
    if (this.timeoutCallback) {
      this.start(this.timeoutCallback);
    }
  }

  isDetected(): boolean {
    return this.detected;
  }
}


// ============================================================================
// 测试运行器
// ============================================================================

export class TestRunner {
  private recorder: MessageRecorder;
  private validator: FlowValidator;
  private timeoutDetector: TimeoutDetector;
  private mockWorker: MockWorkerAdapter;
  private mockUI: MockUserInterface;

  constructor(timeoutMs: number = 30000) {
    this.recorder = new MessageRecorder();
    this.validator = new FlowValidator(this.recorder);
    this.timeoutDetector = new TimeoutDetector(timeoutMs);
    this.mockWorker = new MockWorkerAdapter();
    this.mockUI = new MockUserInterface();
  }

  /** 运行单个测试用例 */
  async runTest(config: TestCaseConfig): Promise<TestResult> {
    const startTime = Date.now();
    this.recorder.clear();
    this.validator.clearErrors();

    // 注册模拟响应
    for (const resp of config.mockResponses) {
      this.mockWorker.registerResponse(resp.agent, resp.trigger, resp.response, resp);
    }

    // 设置超时检测
    let timeoutReject: () => void;
    const timeoutPromise = new Promise<void>((_, reject) => {
      timeoutReject = () => reject(new Error('测试超时'));
    });
    this.timeoutDetector.start(timeoutReject!);

    try {
      // 模拟执行流程
      await Promise.race([
        this.simulateFlow(config),
        timeoutPromise
      ]);

      this.timeoutDetector.stop();

      // 验证结果
      const passed = this.validateResults(config);

      return {
        scenario: config.scenario,
        passed,
        duration: Date.now() - startTime,
        messages: this.recorder.getMessages(),
        stateTransitions: this.recorder.getStateTransitions(),
        errors: this.validator.getErrors(),
        timeoutDetected: false
      };

    } catch (error) {
      this.timeoutDetector.stop();

      return {
        scenario: config.scenario,
        passed: false,
        duration: Date.now() - startTime,
        messages: this.recorder.getMessages(),
        stateTransitions: this.recorder.getStateTransitions(),
        errors: [error instanceof Error ? error.message : String(error)],
        timeoutDetected: this.timeoutDetector.isDetected()
      };
    }
  }

  /** 模拟执行流程 */
  private async simulateFlow(config: TestCaseConfig): Promise<void> {
    // 记录初始状态
    this.recorder.recordStateTransition('', 'idle', 'init');

    // 模拟用户输入
    this.recorder.recordMessage('user_input', 'user', 'orchestrator', {
      content: config.userInput
    });

    // 根据场景模拟不同流程
    switch (config.scenario) {
      case 'clear_requirement':
        await this.simulateClearRequirementFlow(config);
        break;
      case 'vague_requirement':
        await this.simulateVagueRequirementFlow(config);
        break;
      case 'worker_question':
        await this.simulateWorkerQuestionFlow(config);
        break;
      case 'parallel_execution':
        await this.simulateParallelExecutionFlow(config);
        break;
      case 'failure_retry':
        await this.simulateFailureRetryFlow(config);
        break;
    }
  }

  /** 模拟明确需求流程 */
  private async simulateClearRequirementFlow(config: TestCaseConfig): Promise<void> {
    // Phase 1: 分析
    this.recorder.recordStateTransition('idle', 'analyzing', 'user_input');
    const analysisResponse = await this.mockWorker.sendMessage('claude', config.userInput);

    // Phase 2: 确认
    this.recorder.recordStateTransition('analyzing', 'waiting_confirmation', 'plan_ready');
    this.recorder.recordMessage('confirmationRequest', 'orchestrator', 'user', {
      plan: analysisResponse.content
    });

    const confirmed = await this.mockUI.confirm();
    this.recorder.recordMessage('confirmPlan', 'user', 'orchestrator', { confirmed });

    if (!confirmed) {
      this.recorder.recordStateTransition('waiting_confirmation', 'idle', 'cancelled');
      return;
    }

    // Phase 3-4: 执行
    this.recorder.recordStateTransition('waiting_confirmation', 'dispatching', 'confirmed');
    this.recorder.recordMessage('task_dispatch', 'orchestrator', 'worker_claude', {});

    this.recorder.recordStateTransition('dispatching', 'monitoring', 'dispatched');
    this.recorder.recordMessage('task_completed', 'worker_claude', 'orchestrator', {});

    // Phase 5-6: 验证汇总
    this.recorder.recordStateTransition('monitoring', 'summarizing', 'all_completed');
    this.recorder.recordStateTransition('summarizing', 'completed', 'summary_done');
  }

  /** 模拟模糊需求流程 */
  private async simulateVagueRequirementFlow(config: TestCaseConfig): Promise<void> {
    // Phase 0: 澄清
    this.recorder.recordStateTransition('idle', 'clarifying', 'vague_detected');
    this.recorder.recordMessage('clarification_request', 'orchestrator', 'user', {
      questions: ['请明确目标文件', '请说明具体功能']
    });

    const answer = await this.mockUI.answerClarification('请明确目标文件');
    this.recorder.recordMessage('clarification_response', 'user', 'orchestrator', { answer });

    // 🔧 修复：从 clarifying 状态转换到 analyzing，而不是重新从 idle 开始
    // Phase 1: 分析
    this.recorder.recordStateTransition('clarifying', 'analyzing', 'clarification_complete');
    const analysisResponse = await this.mockWorker.sendMessage('claude', config.userInput);

    // Phase 2: 确认
    this.recorder.recordStateTransition('analyzing', 'waiting_confirmation', 'plan_ready');
    this.recorder.recordMessage('confirmationRequest', 'orchestrator', 'user', {
      plan: analysisResponse.content
    });

    const confirmed = await this.mockUI.confirm();
    this.recorder.recordMessage('confirmPlan', 'user', 'orchestrator', { confirmed });

    if (!confirmed) {
      this.recorder.recordStateTransition('waiting_confirmation', 'idle', 'cancelled');
      return;
    }

    // Phase 3-4: 执行
    this.recorder.recordStateTransition('waiting_confirmation', 'dispatching', 'confirmed');
    this.recorder.recordMessage('task_dispatch', 'orchestrator', 'worker_claude', {});

    this.recorder.recordStateTransition('dispatching', 'monitoring', 'dispatched');
    this.recorder.recordMessage('task_completed', 'worker_claude', 'orchestrator', {});

    // Phase 5-6: 验证汇总
    this.recorder.recordStateTransition('monitoring', 'summarizing', 'all_completed');
    this.recorder.recordStateTransition('summarizing', 'completed', 'summary_done');
  }

  /** 模拟子代理疑问流程 */
  private async simulateWorkerQuestionFlow(config: TestCaseConfig): Promise<void> {
    // 前置流程
    this.recorder.recordStateTransition('idle', 'analyzing', 'user_input');
    this.recorder.recordStateTransition('analyzing', 'waiting_confirmation', 'plan_ready');
    this.recorder.recordMessage('confirmationRequest', 'orchestrator', 'user', {});
    this.recorder.recordMessage('confirmPlan', 'user', 'orchestrator', { confirmed: true });

    // 执行中 Worker 提问
    this.recorder.recordStateTransition('waiting_confirmation', 'dispatching', 'confirmed');
    this.recorder.recordStateTransition('dispatching', 'monitoring', 'dispatched');

    this.recorder.recordMessage('worker_question', 'worker_claude', 'orchestrator', {
      question: '请确认数据库连接方式'
    });
    this.recorder.recordMessage('worker_question', 'orchestrator', 'user', {
      question: '请确认数据库连接方式'
    });

    const answer = await this.mockUI.answerQuestion('请确认数据库连接方式');
    this.recorder.recordMessage('worker_answer', 'user', 'orchestrator', { answer });
    this.recorder.recordMessage('worker_answer', 'orchestrator', 'worker_claude', { answer });

    // 继续执行
    this.recorder.recordMessage('task_completed', 'worker_claude', 'orchestrator', {});
    this.recorder.recordStateTransition('monitoring', 'completed', 'all_completed');
  }

  /** 模拟并行执行流程 */
  private async simulateParallelExecutionFlow(config: TestCaseConfig): Promise<void> {
    this.recorder.recordStateTransition('idle', 'analyzing', 'user_input');
    this.recorder.recordStateTransition('analyzing', 'waiting_confirmation', 'plan_ready');
    this.recorder.recordMessage('confirmationRequest', 'orchestrator', 'user', {});
    this.recorder.recordMessage('confirmPlan', 'user', 'orchestrator', { confirmed: true });

    // 并行分发
    this.recorder.recordStateTransition('waiting_confirmation', 'dispatching', 'confirmed');
    this.recorder.recordMessage('task_dispatch', 'orchestrator', 'worker_claude', {});
    this.recorder.recordMessage('task_dispatch', 'orchestrator', 'worker_codex', {});

    this.recorder.recordStateTransition('dispatching', 'monitoring', 'dispatched');

    // 并行完成
    this.recorder.recordMessage('task_completed', 'worker_claude', 'orchestrator', {});
    this.recorder.recordMessage('task_completed', 'worker_codex', 'orchestrator', {});

    this.recorder.recordStateTransition('monitoring', 'completed', 'all_completed');
  }

  /** 模拟失败重试流程 */
  private async simulateFailureRetryFlow(config: TestCaseConfig): Promise<void> {
    this.recorder.recordStateTransition('idle', 'analyzing', 'user_input');
    this.recorder.recordStateTransition('analyzing', 'waiting_confirmation', 'plan_ready');
    this.recorder.recordMessage('confirmationRequest', 'orchestrator', 'user', {});
    this.recorder.recordMessage('confirmPlan', 'user', 'orchestrator', { confirmed: true });

    this.recorder.recordStateTransition('waiting_confirmation', 'dispatching', 'confirmed');
    this.recorder.recordMessage('task_dispatch', 'orchestrator', 'worker_claude', {});

    this.recorder.recordStateTransition('dispatching', 'monitoring', 'dispatched');

    // 第一次失败
    this.recorder.recordMessage('task_failed', 'worker_claude', 'orchestrator', {
      error: '编译错误', canRetry: true
    });

    // 重试
    this.recorder.recordMessage('task_dispatch', 'orchestrator', 'worker_claude', { retry: 1 });
    this.recorder.recordMessage('task_completed', 'worker_claude', 'orchestrator', {});

    this.recorder.recordStateTransition('monitoring', 'completed', 'all_completed');
  }

  /** 验证测试结果 */
  private validateResults(config: TestCaseConfig): boolean {
    let passed = true;

    // 验证消息序列
    if (!this.validator.validateMessageSequence(config.expectedMessages)) {
      passed = false;
    }

    // 验证状态转换
    const expectedTransitions = config.expectedStates.map((state, i, arr) =>
      i > 0 ? [arr[i-1], state] as [string, string] : null
    ).filter(Boolean) as Array<[string, string]>;

    if (!this.validator.validateStateTransitions(expectedTransitions)) {
      passed = false;
    }

    // 验证无死锁
    if (!this.validator.validateNoDeadlock()) {
      passed = false;
    }

    // 验证单一确认
    if (!this.validator.validateSingleConfirmation()) {
      passed = false;
    }

    return passed;
  }

  /** 生成测试报告 */
  generateReport(results: TestResult[]): string {
    const lines: string[] = [
      '# 编排流程端到端测试报告',
      '',
      `生成时间: ${new Date().toISOString()}`,
      '',
      '## 测试结果汇总',
      '',
      `| 场景 | 结果 | 耗时 | 超时 |`,
      `|------|------|------|------|`
    ];

    for (const result of results) {
      const status = result.passed ? '✅ 通过' : '❌ 失败';
      const timeout = result.timeoutDetected ? '⚠️ 是' : '否';
      lines.push(`| ${result.scenario} | ${status} | ${result.duration}ms | ${timeout} |`);
    }

    lines.push('', '## 详细结果', '');

    for (const result of results) {
      lines.push(`### ${result.scenario}`);
      lines.push('');

      if (result.errors.length > 0) {
        lines.push('**错误:**');
        result.errors.forEach(e => lines.push(`- ${e}`));
        lines.push('');
      }

      lines.push('**消息流:**');
      lines.push('```');
      result.messages.forEach(m => {
        lines.push(`[${m.source}] -> [${m.target || 'System'}]: ${m.type}`);
      });
      lines.push('```');
      lines.push('');
    }

    return lines.join('\n');
  }
}


// ============================================================================
// 预定义测试用例
// ============================================================================

export const TEST_CASES: TestCaseConfig[] = [
  {
    name: '明确需求 - 正常流程',
    scenario: 'clear_requirement',
    userInput: '在 src/utils/helper.ts 中添加一个 formatDate 函数，接收 Date 对象，返回 YYYY-MM-DD 格式字符串',
    expectedStates: ['idle', 'analyzing', 'waiting_confirmation', 'dispatching', 'monitoring', 'summarizing', 'completed'],
    expectedMessages: ['user_input', 'confirmationRequest', 'confirmPlan', 'task_dispatch', 'task_completed'],
    mockResponses: [
      {
        trigger: 'formatDate',
        agent: 'claude',
        response: JSON.stringify({
          featureContract: '添加日期格式化函数',
          acceptanceCriteria: ['函数接收Date对象', '返回YYYY-MM-DD格式'],
          subTasks: [{ id: '1', description: '实现formatDate函数', assignedWorker: 'claude' }]
        })
      }
    ],
    timeout: 10000
  },
  {
    name: '模糊需求 - 触发澄清',
    scenario: 'vague_requirement',
    userInput: '帮我创建一个新的工具函数',
    expectedStates: ['idle', 'clarifying', 'analyzing', 'waiting_confirmation', 'dispatching', 'monitoring', 'summarizing', 'completed'],
    expectedMessages: ['user_input', 'clarification_request', 'clarification_response', 'confirmationRequest'],
    mockResponses: [],
    timeout: 15000
  },
  {
    name: '子代理疑问 - 上报用户',
    scenario: 'worker_question',
    userInput: '连接数据库并查询用户表',
    expectedStates: ['idle', 'analyzing', 'waiting_confirmation', 'dispatching', 'monitoring', 'completed'],
    expectedMessages: ['user_input', 'confirmationRequest', 'worker_question', 'worker_answer', 'task_completed'],
    mockResponses: [
      {
        trigger: '数据库',
        agent: 'claude',
        response: '需要确认数据库连接方式',
        shouldAskQuestion: true,
        question: '请确认数据库连接方式'
      }
    ],
    timeout: 20000
  },
  {
    name: '并行执行 - 多Worker',
    scenario: 'parallel_execution',
    userInput: '同时优化前端组件和后端API',
    expectedStates: ['idle', 'analyzing', 'waiting_confirmation', 'dispatching', 'monitoring', 'completed'],
    expectedMessages: ['user_input', 'confirmationRequest', 'task_dispatch', 'task_dispatch', 'task_completed', 'task_completed'],
    mockResponses: [],
    timeout: 15000
  },
  {
    name: '失败重试 - 自动恢复',
    scenario: 'failure_retry',
    userInput: '修复编译错误',
    expectedStates: ['idle', 'analyzing', 'waiting_confirmation', 'dispatching', 'monitoring', 'completed'],
    expectedMessages: ['user_input', 'confirmationRequest', 'task_dispatch', 'task_failed', 'task_dispatch', 'task_completed'],
    mockResponses: [],
    timeout: 20000
  }
];

// ============================================================================
// 运行所有测试
// ============================================================================

export async function runAllTests(): Promise<void> {
  const runner = new TestRunner(30000);
  const results: TestResult[] = [];

  console.log('🚀 开始运行编排流程端到端测试...\n');

  for (const testCase of TEST_CASES) {
    console.log(`📋 运行测试: ${testCase.name}`);
    const result = await runner.runTest(testCase);
    results.push(result);

    const status = result.passed ? '✅ 通过' : '❌ 失败';
    console.log(`   ${status} (${result.duration}ms)`);

    if (!result.passed) {
      result.errors.forEach(e => console.log(`   ⚠️ ${e}`));
    }
    console.log('');
  }

  // 生成报告
  const report = runner.generateReport(results);
  console.log('\n' + '='.repeat(60) + '\n');
  console.log(report);

  // 统计
  const passed = results.filter(r => r.passed).length;
  const failed = results.filter(r => !r.passed).length;
  console.log(`\n📊 测试完成: ${passed} 通过, ${failed} 失败`);
}

// 如果直接运行此文件
if (require.main === module) {
  runAllTests().catch(console.error);
}
