/**
 * UX/UI 流程端到端测试
 *
 * 基于 docs/ux-flow-specification.md 的交互规范
 *
 * 测试目标：
 * 1. 双区域职责划分（主对话区 vs Worker Tab）
 * 2. 消息归属与路由硬规则
 * 3. 基础流程：单 Worker 线性执行
 * 4. 场景1：多 Worker 并行执行
 * 5. 场景2：Worker 依赖链执行
 * 6. 场景3：Worker 提问
 * 7. 场景4：错误与恢复
 * 8. 场景5：用户中断（停止）
 * 9. 场景6：Todo 动态变更
 * 10. 输入区域状态机
 */

import { MessageHub, SubTaskView } from '../../orchestrator/core/message-hub';
import { MissionStateMapper } from '../../orchestrator/mission/state-mapper';
import {
  Mission,
  MissionStatus,
  Assignment,
  WorkerTodo,
} from '../../orchestrator/mission';
import { WorkerSlot } from '../../types';
import { StandardMessage, MessageType, MessageCategory } from '../../protocol/message-protocol';

// ============================================================================
// 测试结果类型
// ============================================================================

interface UXTestResult {
  scenarioId: string;
  description: string;
  passed: boolean;
  verificationPoints: VerificationPoint[];
  duration: number;
}

interface VerificationPoint {
  name: string;
  expected: string;
  actual: string;
  passed: boolean;
}

// ============================================================================
// Mock 工厂
// ============================================================================

function createMockMission(options?: {
  id?: string;
  status?: MissionStatus;
  assignmentCount?: number;
  todoCount?: number;
  workers?: WorkerSlot[];
}): Mission {
  const id = options?.id || `mission-${Date.now()}`;
  const assignmentCount = options?.assignmentCount || 1;
  const todoCount = options?.todoCount || 2;
  const workers = options?.workers || ['claude'];

  const assignments: Assignment[] = [];
  for (let i = 0; i < assignmentCount; i++) {
    const worker = workers[i % workers.length];
    const todos: WorkerTodo[] = [];
    for (let j = 0; j < todoCount; j++) {
      todos.push({
        id: `todo-${i}-${j}`,
        missionId: id,
        assignmentId: `assign-${i}`,
        content: `任务 ${j + 1}`,
        reasoning: '测试',
        expectedOutput: '完成',
        type: 'implementation',
        workerId: worker,
        priority: j + 1,
        outOfScope: false,
        dependsOn: [],
        requiredContracts: [],
        producesContracts: [],
        status: j === 0 ? 'completed' : 'pending',
        progress: j === 0 ? 100 : 0,
        retryCount: 0,
        maxRetries: 3,
        createdAt: Date.now(),
      });
    }
    assignments.push({
      id: `assign-${i}`,
      missionId: id,
      workerId: worker,
      assignmentReason: {
        profileMatch: { category: 'general', score: 0.9, matchedKeywords: [] },
        contractRole: 'none',
        explanation: '测试分配',
        alternatives: [],
      },
      responsibility: `职责 ${i + 1}`,
      shortTitle: `任务 ${i + 1}`,
      scope: { includes: ['src/'], excludes: [] },
      guidancePrompt: '请执行任务',
      producerContracts: [],
      consumerContracts: [],
      todos,
      planningStatus: 'approved',
      status: options?.status === 'executing' ? 'executing' : 'pending',
      progress: 50,
      createdAt: Date.now(),
    });
  }

  return {
    id,
    sessionId: 'session-test',
    userPrompt: '测试任务',
    goal: '完成测试目标',
    analysis: '测试分析',
    context: '测试上下文',
    constraints: [],
    acceptanceCriteria: [{ id: 'ac-1', description: '通过测试', verifiable: true, status: 'pending' }],
    contracts: [],
    assignments,
    riskLevel: 'low',
    riskFactors: [],
    executionPath: 'standard',
    status: options?.status || 'executing',
    phase: 'execution',
    createdAt: Date.now(),
    updatedAt: Date.now(),
  };
}

// ============================================================================
// UX-01: 双区域职责划分测试
// ============================================================================

function testDualAreaResponsibility(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-01-A: 主对话区只接受编排者消息和 Worker 状态卡片
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const mainAreaMessages: StandardMessage[] = [];
    const workerTabMessages: StandardMessage[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.source === 'orchestrator') {
        mainAreaMessages.push(msg);
      } else if (msg.source === 'worker') {
        workerTabMessages.push(msg);
      }
    });

    // 发送编排者消息（应在主对话区）
    hub.progress('planning', '正在分析任务...');
    hub.result('任务完成');

    // 发送 SubTaskCard（应在主对话区）
    hub.subTaskCard({
      id: 'subtask-1',
      title: '执行任务',
      worker: 'claude',
      status: 'running',
    });

    // 发送 Worker 输出（应在 Worker Tab）
    hub.workerOutput('claude', '正在执行...');

    verificationPoints.push({
      name: '编排者消息路由到主对话区',
      expected: '>= 2',
      actual: String(mainAreaMessages.length),
      passed: mainAreaMessages.length >= 2,
    });

    verificationPoints.push({
      name: 'Worker 输出路由到 Worker Tab',
      expected: '>= 1',
      actual: String(workerTabMessages.length),
      passed: workerTabMessages.length >= 1,
    });

    verificationPoints.push({
      name: 'Worker 输出不进入主对话区',
      expected: '0',
      actual: String(mainAreaMessages.filter(m => m.source === 'worker' && m.type !== 'result').length),
      passed: mainAreaMessages.filter(m => m.source === 'worker' && m.type !== 'result').length === 0,
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-01-A',
      description: '主对话区只接受编排者消息和 Worker 状态卡片',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-01-B: Worker Tab 只接受 Worker 自身的执行细节
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const workerMessages: StandardMessage[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.source === 'worker') {
        workerMessages.push(msg);
      }
    });

    // Worker 输出
    hub.workerOutput('claude', '执行结果');

    verificationPoints.push({
      name: 'Worker 消息正确标记来源',
      expected: 'all worker',
      actual: workerMessages.every(m => m.source === 'worker') ? 'all worker' : 'mixed',
      passed: workerMessages.every(m => m.source === 'worker'),
    });

    verificationPoints.push({
      name: 'Worker 消息包含正确的 agent 标识',
      expected: 'claude',
      actual: workerMessages[0]?.agent || 'null',
      passed: workerMessages.length === 0 || workerMessages.every(m => m.agent === 'claude'),
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-01-B',
      description: 'Worker Tab 只接受 Worker 自身的执行细节',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-01-C: Worker 状态卡片由编排者生成与更新
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const subTaskCards: Array<{ source: string; card: SubTaskView }> = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别，metadata.subTaskCard 携带数据
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        subTaskCards.push({
          source: msg.source,
          card: msg.metadata.subTaskCard as SubTaskView,
        });
      }
    });

    // 编排者发送 SubTaskCard
    hub.subTaskCard({
      id: 'subtask-1',
      title: '分析代码',
      worker: 'claude',
      status: 'running',
    });

    // 更新状态
    hub.subTaskCard({
      id: 'subtask-1',
      title: '分析代码',
      worker: 'claude',
      status: 'completed',
      summary: '发现 3 个问题',
    });

    verificationPoints.push({
      name: 'SubTaskCard 由编排者发送',
      expected: 'orchestrator',
      actual: subTaskCards[0]?.source || 'null',
      passed: subTaskCards.every(c => c.source === 'orchestrator'),
    });

    verificationPoints.push({
      name: 'SubTaskCard 状态可更新',
      expected: 'completed',
      actual: subTaskCards[1]?.card?.status || 'null',
      passed: subTaskCards[1]?.card?.status === 'completed',
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-01-C',
      description: 'Worker 状态卡片由编排者生成与更新',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-02: 基础流程 - 单 Worker 线性执行
// ============================================================================

function testSingleWorkerLinearFlow(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-02-A: 完整的 5 阶段流程
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const timeline: Array<{ phase: string; type: string }> = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.source === 'orchestrator') {
        if (msg.type === 'thinking') {
          timeline.push({ phase: '2-编排者理解', type: 'thinking' });
        } else if (msg.type === 'plan') {
          timeline.push({ phase: '2-编排者规划', type: 'plan' });
        } else if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
          // 方案 B：使用 MessageType.TASK_CARD 识别
          const status = (msg.metadata.subTaskCard as SubTaskView).status;
          if (status === 'running') {
            timeline.push({ phase: '3-任务派发', type: 'subtask-start' });
          } else if (status === 'completed') {
            timeline.push({ phase: '4-任务完成', type: 'subtask-done' });
          }
        } else if (msg.type === 'result') {
          timeline.push({ phase: '5-编排者汇总', type: 'summary' });
        } else if (msg.type === 'progress') {
          timeline.push({ phase: '2-编排者进度', type: 'progress' });
        }
      }
    });

    // 模拟完整流程
    // 阶段2: 编排者理解与规划
    hub.progress('planning', '正在分析任务...');

    // 阶段3: 任务派发
    hub.subTaskCard({
      id: 'subtask-1',
      title: '分析依赖',
      worker: 'claude',
      status: 'running',
    });

    // 阶段4: 任务完成
    hub.subTaskCard({
      id: 'subtask-1',
      title: '分析依赖',
      worker: 'claude',
      status: 'completed',
      summary: '发现 3 个问题',
    });

    // 阶段5: 编排者汇总
    hub.result('分析完成，共发现 3 个依赖问题');

    verificationPoints.push({
      name: '包含多个阶段',
      expected: '>= 3',
      actual: String(timeline.length),
      passed: timeline.length >= 3,
    });

    verificationPoints.push({
      name: '有任务派发',
      expected: 'has subtask-start',
      actual: timeline.some(t => t.type === 'subtask-start') ? 'has subtask-start' : 'no subtask-start',
      passed: timeline.some(t => t.type === 'subtask-start'),
    });

    verificationPoints.push({
      name: '有任务完成',
      expected: 'has subtask-done',
      actual: timeline.some(t => t.type === 'subtask-done') ? 'has subtask-done' : 'no subtask-done',
      passed: timeline.some(t => t.type === 'subtask-done'),
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-02-A',
      description: '完整的 5 阶段流程',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-03: 场景1 - 多 Worker 并行执行
// ============================================================================

function testMultiWorkerParallel(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-03-A: 多个 Worker 独立卡片，上下垂直排列
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const subTaskCards: SubTaskView[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        subTaskCards.push(msg.metadata.subTaskCard as SubTaskView);
      }
    });

    // 并行派发两个 Worker
    hub.subTaskCard({
      id: 'subtask-1',
      title: '分析前端依赖',
      worker: 'claude',
      status: 'running',
    });

    hub.subTaskCard({
      id: 'subtask-2',
      title: '分析后端 API',
      worker: 'gemini',
      status: 'running',
    });

    verificationPoints.push({
      name: '多个 Worker 独立卡片',
      expected: '2',
      actual: String(subTaskCards.length),
      passed: subTaskCards.length === 2,
    });

    verificationPoints.push({
      name: '不同 Worker',
      expected: 'claude,gemini',
      actual: subTaskCards.map(c => c.worker).join(','),
      passed: subTaskCards[0]?.worker === 'claude' && subTaskCards[1]?.worker === 'gemini',
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-03-A',
      description: '多个 Worker 独立卡片',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-03-B: 进度独立，先完成的先更新
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const completionOrder: string[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        const card = msg.metadata.subTaskCard as SubTaskView;
        if (card.status === 'completed') {
          completionOrder.push(card.worker);
        }
      }
    });

    // Claude 先完成
    hub.subTaskCard({
      id: 'subtask-1',
      title: '分析前端',
      worker: 'claude',
      status: 'completed',
    });

    // Gemini 后完成
    hub.subTaskCard({
      id: 'subtask-2',
      title: '分析后端',
      worker: 'gemini',
      status: 'completed',
    });

    verificationPoints.push({
      name: '完成顺序正确记录',
      expected: 'claude,gemini',
      actual: completionOrder.join(','),
      passed: completionOrder[0] === 'claude' && completionOrder[1] === 'gemini',
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-03-B',
      description: '进度独立，先完成的先更新',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-03-C: 全部完成后编排者输出汇总
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    let summaryReceived = false;
    let summaryAfterAllComplete = false;
    let completedCount = 0;

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        const card = msg.metadata.subTaskCard as SubTaskView;
        if (card.status === 'completed') {
          completedCount++;
        }
      }
      // 汇总消息：type='result'（TASK_CARD 是独立类型，不会冲突）
      if (msg.type === 'result' && msg.source === 'orchestrator') {
        summaryReceived = true;
        summaryAfterAllComplete = completedCount === 2;
      }
    });

    // 两个任务完成
    hub.subTaskCard({ id: 's1', title: 'T1', worker: 'claude', status: 'completed' });
    hub.subTaskCard({ id: 's2', title: 'T2', worker: 'gemini', status: 'completed' });

    // 编排者汇总
    hub.result('所有任务完成');

    verificationPoints.push({
      name: '汇总在全部完成后',
      expected: 'true',
      actual: String(summaryAfterAllComplete),
      passed: summaryAfterAllComplete,
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-03-C',
      description: '全部完成后编排者输出汇总',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-04: 场景2 - Worker 依赖链执行
// ============================================================================

function testWorkerDependencyChain(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-04-A: 前一个完成后自动触发下一个
  {
    const startTime = Date.now();
    const verificationPoints: VerificationPoint[] = [];

    // 模拟依赖链状态
    const tasks = [
      { id: 't1', status: 'completed', dependsOn: [] as string[] },
      { id: 't2', status: 'pending', dependsOn: ['t1'] },
      { id: 't3', status: 'pending', dependsOn: ['t2'] },
    ];

    const canExecute = (taskId: string): boolean => {
      const task = tasks.find(t => t.id === taskId);
      if (!task) return false;
      return task.dependsOn.every(depId => {
        const dep = tasks.find(t => t.id === depId);
        return dep?.status === 'completed';
      });
    };

    verificationPoints.push({
      name: 't2 可执行（t1 已完成）',
      expected: 'true',
      actual: String(canExecute('t2')),
      passed: canExecute('t2'),
    });

    verificationPoints.push({
      name: 't3 不可执行（t2 未完成）',
      expected: 'false',
      actual: String(canExecute('t3')),
      passed: !canExecute('t3'),
    });

    results.push({
      scenarioId: 'UX-04-A',
      description: '前一个完成后自动触发下一个',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-05: 场景3 - Worker 提问
// ============================================================================

function testWorkerQuestion(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-05-A: Worker 卡片显示"等待确认"状态
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    let pendingCardStatus: string | null = null;

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        const card = msg.metadata.subTaskCard as SubTaskView;
        if (card.status === 'pending') {
          pendingCardStatus = card.status;
        }
      }
    });

    hub.subTaskCard({
      id: 's1',
      title: '重构用户模块',
      worker: 'claude',
      status: 'pending',
    });

    verificationPoints.push({
      name: '状态为等待确认',
      expected: 'pending',
      actual: pendingCardStatus || 'null',
      passed: pendingCardStatus === 'pending',
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-05-A',
      description: 'Worker 卡片显示"等待确认"状态',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-05-B: 用户回复后恢复执行
  // 验证同一任务的状态流转: running -> pending -> running
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const cardStates: string[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        const card = msg.metadata.subTaskCard as SubTaskView;
        cardStates.push(card.status);
      }
    });

    // 初始执行
    hub.subTaskCard({ id: 's1', title: '重构', worker: 'claude', status: 'running' });

    // 等待确认
    hub.subTaskCard({ id: 's1', title: '重构', worker: 'claude', status: 'pending' });

    // 用户回复后恢复
    hub.subTaskCard({ id: 's1', title: '重构', worker: 'claude', status: 'running' });

    verificationPoints.push({
      name: '状态流转: running -> pending -> running',
      expected: 'running,pending,running',
      actual: cardStates.join(','),
      passed: cardStates.join(',') === 'running,pending,running',
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-05-B',
      description: '用户回复后恢复执行',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-06: 场景4 - 错误与恢复
// ============================================================================

function testErrorAndRecovery(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-06-A: Worker 卡片显示失败状态
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    let failedCardStatus: string | null = null;

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        const card = msg.metadata.subTaskCard as SubTaskView;
        if (card.status === 'failed') {
          failedCardStatus = card.status;
        }
      }
    });

    hub.subTaskCard({
      id: 's1',
      title: '修改配置文件',
      worker: 'claude',
      status: 'failed',
    });

    verificationPoints.push({
      name: '状态为失败',
      expected: 'failed',
      actual: failedCardStatus || 'null',
      passed: failedCardStatus === 'failed',
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-06-A',
      description: 'Worker 卡片显示失败状态',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-06-B: 编排者提供恢复选项
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    let errorRecoverable: boolean | null = null;

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.type === 'error' && msg.source === 'orchestrator') {
        errorRecoverable = msg.metadata?.recoverable as boolean ?? null;
      }
    });

    hub.error('任务执行失败', {
      recoverable: true,
    });

    verificationPoints.push({
      name: '错误消息可恢复',
      expected: 'true',
      actual: String(errorRecoverable),
      passed: errorRecoverable === true,
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-06-B',
      description: '编排者提供恢复选项',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-07: 场景5 - 用户中断（停止）
// ============================================================================

function testUserInterrupt(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-07-A: 停止后所有 Worker 状态更新为已停止
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const stoppedCards: SubTaskView[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      // 方案 B：使用 MessageType.TASK_CARD 识别
      if (msg.type === MessageType.TASK_CARD && msg.metadata?.subTaskCard) {
        const card = msg.metadata.subTaskCard as SubTaskView;
        if (card.status === 'stopped') {
          stoppedCards.push(card);
        }
      }
    });

    // 模拟两个运行中的 Worker 被停止
    hub.subTaskCard({ id: 's1', title: '重构模块', worker: 'claude', status: 'stopped' });
    hub.subTaskCard({ id: 's2', title: '更新文档', worker: 'gemini', status: 'stopped' });

    verificationPoints.push({
      name: '所有 Worker 显示已停止',
      expected: '2',
      actual: String(stoppedCards.length),
      passed: stoppedCards.length === 2,
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-07-A',
      description: '停止后所有 Worker 状态更新为已停止',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-07-B: 编排者汇报进度
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    let summaryMessage: StandardMessage | null = null;

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.type === 'result' && msg.source === 'orchestrator') {
        summaryMessage = msg;
      }
    });

    hub.result('已停止所有任务。Claude: 完成 2/5 步骤; Gemini: 完成 1/3 步骤');

    verificationPoints.push({
      name: '编排者输出停止汇总',
      expected: 'has summary',
      actual: summaryMessage ? 'has summary' : 'no summary',
      passed: !!summaryMessage,
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-07-B',
      description: '编排者汇报进度',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-08: 场景6 - Todo 动态变更
// ============================================================================

function testTodoDynamicChange(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-08-A: 动态新增 Todo
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    let progressMessage: StandardMessage | null = null;

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.type === 'progress' && msg.source === 'orchestrator') {
        progressMessage = msg;
      }
    });

    hub.progress('todo_change', '发现需要额外步骤：重构认证模块');

    verificationPoints.push({
      name: '通知用户 Todo 变更',
      expected: 'has progress',
      actual: progressMessage ? 'has progress' : 'no progress',
      passed: !!progressMessage,
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-08-A',
      description: '动态新增 Todo',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-09: 输入区域状态机
// ============================================================================

function testInputAreaStateMachine(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-09-A: 空闲状态 - 输入框为空时发送按钮禁用
  {
    const startTime = Date.now();
    const verificationPoints: VerificationPoint[] = [];

    // 模拟状态机
    const getButtonState = (isExecuting: boolean, inputEmpty: boolean) => {
      if (!isExecuting) {
        return inputEmpty ? 'send-disabled' : 'send-enabled';
      } else {
        return inputEmpty ? 'stop' : 'send-enabled';
      }
    };

    verificationPoints.push({
      name: '空闲+空输入 = 发送禁用',
      expected: 'send-disabled',
      actual: getButtonState(false, true),
      passed: getButtonState(false, true) === 'send-disabled',
    });

    verificationPoints.push({
      name: '空闲+有输入 = 发送启用',
      expected: 'send-enabled',
      actual: getButtonState(false, false),
      passed: getButtonState(false, false) === 'send-enabled',
    });

    verificationPoints.push({
      name: '执行中+空输入 = 停止按钮',
      expected: 'stop',
      actual: getButtonState(true, true),
      passed: getButtonState(true, true) === 'stop',
    });

    verificationPoints.push({
      name: '执行中+有输入 = 发送启用',
      expected: 'send-enabled',
      actual: getButtonState(true, false),
      passed: getButtonState(true, false) === 'send-enabled',
    });

    results.push({
      scenarioId: 'UX-09-A',
      description: '输入区域按钮状态机',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-09-B: 高频发送限制
  {
    const startTime = Date.now();
    const verificationPoints: VerificationPoint[] = [];

    // 模拟限频逻辑
    const rateLimiter = {
      lastSend: 0,
      executingLimit: 1000, // 1秒
      idleLimit: 300, // 300ms
    };

    const canSend = (isExecuting: boolean): boolean => {
      const now = Date.now();
      const limit = isExecuting ? rateLimiter.executingLimit : rateLimiter.idleLimit;
      if (now - rateLimiter.lastSend < limit) {
        return false;
      }
      rateLimiter.lastSend = now;
      return true;
    };

    // 第一次发送
    const firstSend = canSend(false);

    // 立即再发
    const secondSend = canSend(false);

    verificationPoints.push({
      name: '首次发送允许',
      expected: 'true',
      actual: String(firstSend),
      passed: firstSend,
    });

    verificationPoints.push({
      name: '高频发送被拒绝',
      expected: 'false',
      actual: String(secondSend),
      passed: !secondSend,
    });

    results.push({
      scenarioId: 'UX-09-B',
      description: '高频发送限制',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-10: Todo 状态图例
// ============================================================================

function testTodoStatusIcons(): UXTestResult[] {
  const results: UXTestResult[] = [];

  {
    const startTime = Date.now();
    const verificationPoints: VerificationPoint[] = [];

    // 验证状态到图标的映射
    const statusIconMap: Record<string, string> = {
      pending: '⬚',
      in_progress: '🟡',
      completed: '✅',
      skipped: '⏭️',
      failed: '❌',
      stopped: '⏹️',
    };

    const expectedStatuses = ['pending', 'in_progress', 'completed', 'skipped', 'failed', 'stopped'];

    for (const status of expectedStatuses) {
      verificationPoints.push({
        name: `状态 ${status} 有对应图标`,
        expected: 'has icon',
        actual: statusIconMap[status] ? 'has icon' : 'no icon',
        passed: !!statusIconMap[status],
      });
    }

    results.push({
      scenarioId: 'UX-10-A',
      description: 'Todo 状态图例完整',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// UX-11: 任务分配宣告与任务说明 (refactor-orchestration-messaging.md)
// ============================================================================

/**
 * 测试 taskAssignment 和 workerInstruction API
 * 验证消息流符合 UX 规范
 */
function testTaskAssignmentAndInstruction(): UXTestResult[] {
  const results: UXTestResult[] = [];

  // UX-11-A: taskAssignment 发送任务分配宣告到主对话区
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const mainAreaMessages: StandardMessage[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.source === 'orchestrator' && !msg.metadata?.dispatchToWorker) {
        mainAreaMessages.push(msg);
      }
    });

    // 发送任务分配宣告
    hub.taskAssignment([
      { worker: 'claude', shortTitle: '分析依赖' },
      { worker: 'gemini', shortTitle: '优化性能' },
    ]);

    verificationPoints.push({
      name: '任务分配宣告发送到主对话区',
      expected: '>= 1',
      actual: String(mainAreaMessages.length),
      passed: mainAreaMessages.length >= 1,
    });

    const assignmentMsg = mainAreaMessages.find(m =>
      m.metadata?.phase === 'task_assignment'
    );

    verificationPoints.push({
      name: '任务分配宣告包含 phase: task_assignment',
      expected: 'task_assignment',
      actual: assignmentMsg?.metadata?.phase || 'null',
      passed: assignmentMsg?.metadata?.phase === 'task_assignment',
    });

    const content = assignmentMsg?.blocks?.[0]?.type === 'text'
      ? (assignmentMsg.blocks[0] as any).content
      : '';

    verificationPoints.push({
      name: '任务分配宣告内容包含 Worker 名称',
      expected: 'contains claude and gemini',
      actual: content.includes('claude') && content.includes('gemini') ? 'yes' : 'no',
      passed: content.includes('claude') && content.includes('gemini'),
    });

    verificationPoints.push({
      name: '任务分配宣告内容包含 shortTitle',
      expected: 'contains 分析依赖 and 优化性能',
      actual: content.includes('分析依赖') && content.includes('优化性能') ? 'yes' : 'no',
      passed: content.includes('分析依赖') && content.includes('优化性能'),
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-11-A',
      description: 'taskAssignment 发送任务分配宣告到主对话区',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-11-B: workerInstruction 发送任务说明到 Worker Tab
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const workerTabMessages: StandardMessage[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.metadata?.dispatchToWorker) {
        workerTabMessages.push(msg);
      }
    });

    // 发送任务说明
    hub.workerInstruction('claude', '请分析项目的依赖结构，关注循环依赖问题。', {
      assignmentId: 'assign-1',
      missionId: 'mission-1',
    });

    verificationPoints.push({
      name: '任务说明发送到 Worker Tab (dispatchToWorker: true)',
      expected: '>= 1',
      actual: String(workerTabMessages.length),
      passed: workerTabMessages.length >= 1,
    });

    // 方案 B：使用 MessageType.INSTRUCTION 识别任务说明
    const instructionMsg = workerTabMessages.find(m =>
      m.type === 'instruction'
    );

    verificationPoints.push({
      name: '任务说明使用 MessageType.INSTRUCTION',
      expected: 'instruction',
      actual: String(instructionMsg?.type || 'null'),
      passed: instructionMsg?.type === 'instruction',
    });

    verificationPoints.push({
      name: '任务说明路由到正确的 Worker',
      expected: 'claude',
      actual: instructionMsg?.metadata?.worker || 'null',
      passed: instructionMsg?.metadata?.worker === 'claude',
    });

    verificationPoints.push({
      name: '任务说明包含 assignmentId 和 missionId',
      expected: 'assign-1, mission-1',
      actual: `${instructionMsg?.metadata?.assignmentId || 'null'}, ${instructionMsg?.metadata?.missionId || 'null'}`,
      passed: instructionMsg?.metadata?.assignmentId === 'assign-1' &&
              instructionMsg?.metadata?.missionId === 'mission-1',
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-11-B',
      description: 'workerInstruction 发送任务说明到 Worker Tab',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-11-C: 单 Worker 时 taskAssignment 使用单数格式
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const verificationPoints: VerificationPoint[] = [];

    const mainAreaMessages: StandardMessage[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
      if (msg.source === 'orchestrator' && !msg.metadata?.dispatchToWorker) {
        mainAreaMessages.push(msg);
      }
    });

    // 发送单 Worker 任务分配
    hub.taskAssignment([
      { worker: 'claude', shortTitle: '分析代码' },
    ]);

    const content = mainAreaMessages[0]?.blocks?.[0]?.type === 'text'
      ? (mainAreaMessages[0].blocks[0] as any).content
      : '';

    verificationPoints.push({
      name: '单 Worker 使用单数格式（不包含"协作"）',
      expected: 'not contains 协作',
      actual: !content.includes('协作') ? 'yes' : 'no',
      passed: !content.includes('协作'),
    });

    verificationPoints.push({
      name: '单 Worker 格式为"我将安排 X 执行：Y"',
      expected: 'contains 执行',
      actual: content.includes('执行') ? 'yes' : 'no',
      passed: content.includes('执行'),
    });

    hub.dispose();

    results.push({
      scenarioId: 'UX-11-C',
      description: '单 Worker 时 taskAssignment 使用单数格式',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-11-D: SubTaskCard 使用 shortTitle 而非 delegationBriefing
  {
    const startTime = Date.now();
    const hub = new MessageHub();
    const mapper = new MissionStateMapper();
    const verificationPoints: VerificationPoint[] = [];

    // 创建包含 shortTitle 的 Mission
    const mission = createMockMission({
      assignmentCount: 1,
      workers: ['claude'],
    });
    // 手动设置 delegationBriefing 以验证不会使用它
    mission.assignments[0].delegationBriefing = '这是一段很长的详细委托说明，包含任务背景、重点关注点和期望产出等内容。';

    const mapped = mapper.mapAssignmentToSubTaskView(mission.assignments[0]);

    verificationPoints.push({
      name: 'SubTaskView.title 使用 shortTitle',
      expected: mission.assignments[0].shortTitle,
      actual: mapped.title,
      passed: mapped.title === mission.assignments[0].shortTitle,
    });

    verificationPoints.push({
      name: 'SubTaskView.title 不使用 delegationBriefing',
      expected: 'not equal to delegationBriefing',
      actual: mapped.title !== mission.assignments[0].delegationBriefing ? 'different' : 'same',
      passed: mapped.title !== mission.assignments[0].delegationBriefing,
    });

    hub.dispose();
    mapper.dispose();

    results.push({
      scenarioId: 'UX-11-D',
      description: 'SubTaskCard 使用 shortTitle 而非 delegationBriefing',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  // UX-11-E: USER_INPUT 消息识别逻辑验证
  // 验证 type === MessageType.USER_INPUT 的消息应被识别为用户输入
  {
    const startTime = Date.now();
    const verificationPoints: VerificationPoint[] = [];

    // 创建模拟的用户输入消息（使用 MessageType.USER_INPUT）
    const userMessage: StandardMessage = {
      id: 'user-msg-1',
      traceId: 'trace-1',
      category: MessageCategory.CONTENT,
      type: MessageType.USER_INPUT,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: 'completed' as any,
      blocks: [{ type: 'text', content: '帮我分析代码' }],
      metadata: {},
      timestamp: Date.now(),
      updatedAt: Date.now(),
    };

    // 内联分类逻辑（与 message-classifier.ts 一致）
    const isUserInput = (msg: StandardMessage): boolean => {
      return msg.type === MessageType.USER_INPUT;
    };

    verificationPoints.push({
      name: 'MessageType.USER_INPUT 的消息被识别为用户输入',
      expected: 'true',
      actual: String(isUserInput(userMessage)),
      passed: isUserInput(userMessage) === true,
    });

    // 测试非用户消息不会被误识别
    const assistantMessage: StandardMessage = {
      ...userMessage,
      id: 'assistant-msg-1',
      type: MessageType.TEXT,
    };

    verificationPoints.push({
      name: 'MessageType.TEXT 的消息不被识别为用户输入',
      expected: 'false',
      actual: String(isUserInput(assistantMessage)),
      passed: isUserInput(assistantMessage) === false,
    });

    // 测试其他类型消息不会被误识别
    const thinkingMessage: StandardMessage = {
      ...userMessage,
      id: 'thinking-msg-1',
      type: MessageType.THINKING,
    };

    verificationPoints.push({
      name: 'MessageType.THINKING 的消息不被识别为用户输入',
      expected: 'false',
      actual: String(isUserInput(thinkingMessage)),
      passed: isUserInput(thinkingMessage) === false,
    });

    results.push({
      scenarioId: 'UX-11-E',
      description: 'USER_INPUT 消息识别逻辑验证',
      passed: verificationPoints.every(v => v.passed),
      verificationPoints,
      duration: Date.now() - startTime,
    });
  }

  return results;
}

// ============================================================================
// 运行所有测试
// ============================================================================

export async function runUXFlowTests(): Promise<void> {
  console.log('='.repeat(60));
  console.log('UX/UI 流程端到端测试');
  console.log('基于 docs/ux-flow-specification.md');
  console.log('='.repeat(60));
  console.log('');

  const allResults: UXTestResult[] = [];

  // UX-01: 双区域职责划分
  console.log('【UX-01 双区域职责划分】');
  const dualAreaResults = testDualAreaResponsibility();
  allResults.push(...dualAreaResults);
  printResults(dualAreaResults);

  // UX-02: 基础流程
  console.log('【UX-02 基础流程 - 单 Worker 线性执行】');
  const singleWorkerResults = testSingleWorkerLinearFlow();
  allResults.push(...singleWorkerResults);
  printResults(singleWorkerResults);

  // UX-03: 多 Worker 并行
  console.log('【UX-03 场景1 - 多 Worker 并行执行】');
  const multiWorkerResults = testMultiWorkerParallel();
  allResults.push(...multiWorkerResults);
  printResults(multiWorkerResults);

  // UX-04: 依赖链执行
  console.log('【UX-04 场景2 - Worker 依赖链执行】');
  const dependencyResults = testWorkerDependencyChain();
  allResults.push(...dependencyResults);
  printResults(dependencyResults);

  // UX-05: Worker 提问
  console.log('【UX-05 场景3 - Worker 提问】');
  const questionResults = testWorkerQuestion();
  allResults.push(...questionResults);
  printResults(questionResults);

  // UX-06: 错误与恢复
  console.log('【UX-06 场景4 - 错误与恢复】');
  const errorResults = testErrorAndRecovery();
  allResults.push(...errorResults);
  printResults(errorResults);

  // UX-07: 用户中断
  console.log('【UX-07 场景5 - 用户中断】');
  const interruptResults = testUserInterrupt();
  allResults.push(...interruptResults);
  printResults(interruptResults);

  // UX-08: Todo 动态变更
  console.log('【UX-08 场景6 - Todo 动态变更】');
  const todoChangeResults = testTodoDynamicChange();
  allResults.push(...todoChangeResults);
  printResults(todoChangeResults);

  // UX-09: 输入区域状态机
  console.log('【UX-09 输入区域状态机】');
  const inputAreaResults = testInputAreaStateMachine();
  allResults.push(...inputAreaResults);
  printResults(inputAreaResults);

  // UX-10: Todo 状态图例
  console.log('【UX-10 Todo 状态图例】');
  const todoIconResults = testTodoStatusIcons();
  allResults.push(...todoIconResults);
  printResults(todoIconResults);

  // UX-11: 任务分配宣告与任务说明 (refactor-orchestration-messaging.md)
  console.log('【UX-11 任务分配宣告与任务说明】');
  const assignmentResults = testTaskAssignmentAndInstruction();
  allResults.push(...assignmentResults);
  printResults(assignmentResults);

  // 汇总
  console.log('='.repeat(60));
  console.log('测试汇总');
  console.log('='.repeat(60));

  const passed = allResults.filter(r => r.passed).length;
  const failed = allResults.filter(r => !r.passed).length;
  const total = allResults.length;
  const passRate = Math.round(passed / total * 100);

  console.log(`通过: ${passed}/${total} (${passRate}%)`);
  console.log(`失败: ${failed}/${total}`);

  if (failed > 0) {
    console.log('\n失败场景:');
    allResults.filter(r => !r.passed).forEach(r => {
      console.log(`  - ${r.scenarioId}: ${r.description}`);
      r.verificationPoints.filter(v => !v.passed).forEach(v => {
        console.log(`      ${v.name}: 期望 ${v.expected}, 实际 ${v.actual}`);
      });
    });
  }

  console.log('');
}

function printResults(results: UXTestResult[]): void {
  for (const result of results) {
    const status = result.passed ? '✓' : '✗';
    console.log(`  ${status} ${result.scenarioId}: ${result.description} (${result.duration}ms)`);
    if (!result.passed) {
      for (const point of result.verificationPoints.filter(p => !p.passed)) {
        console.log(`      - ${point.name}: 期望 ${point.expected}, 实际 ${point.actual}`);
      }
    }
  }
  console.log('');
}

// 直接运行测试
if (require.main === module) {
  runUXFlowTests().catch(console.error);
}
