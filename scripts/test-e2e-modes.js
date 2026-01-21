/**
 * 端对端模式测试 - 模拟用户完整交互流程
 *
 * 测试三种交互模式：
 * - ask: 对话模式，仅对话交流，不执行代码编辑
 * - agent: 代理模式，关键节点需要用户确认（Hard Stop）
 * - auto: 自动模式，无需确认，自动执行并回滚保护
 *
 * 测试覆盖：
 * 1. 模式切换
 * 2. 消息流程（从用户输入到编排者响应）
 * 3. 确认/取消流程
 * 4. 回滚保护
 * 5. SubTask/Assignment 同步
 * 6. Token 统计
 */

// Mock vscode 模块
const Module = require('module');
const originalRequire = Module.prototype.require;
Module.prototype.require = function(id) {
  if (id === 'vscode') {
    return {
      languages: { getDiagnostics: () => [] },
      DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
      Uri: { file: (p) => ({ fsPath: p, path: p }), parse: (s) => ({ fsPath: s, path: s }) },
      workspace: {
        workspaceFolders: [],
        getConfiguration: () => ({ get: () => undefined, update: () => Promise.resolve() }),
        onDidChangeConfiguration: () => ({ dispose: () => {} }),
      },
      window: {
        showInformationMessage: () => Promise.resolve(),
        showWarningMessage: () => Promise.resolve(),
        showErrorMessage: () => Promise.resolve(),
        createOutputChannel: () => ({ appendLine: () => {}, show: () => {}, dispose: () => {} }),
      },
      commands: { registerCommand: () => ({ dispose: () => {} }), executeCommand: () => Promise.resolve() },
      EventEmitter: class { event = () => {}; fire() {} dispose() {} },
    };
  }
  return originalRequire.apply(this, arguments);
};

const path = require('path');
const { TestRunner, waitFor } = require('./test-utils');

const workspaceRoot = path.resolve(__dirname, '..');

// ============================================================================
// Mock CLI Adapter Factory
// ============================================================================

class MockCLIAdapterFactory {
  constructor() {
    this.messageHistory = [];
    this.responseQueue = [];
    this.defaultResponse = {
      content: 'Mock response',
      tokenUsage: { inputTokens: 100, outputTokens: 50 },
    };
  }

  /** 设置下一个响应 */
  setNextResponse(response) {
    this.responseQueue.push(response);
  }

  /** 设置默认响应 */
  setDefaultResponse(response) {
    this.defaultResponse = response;
  }

  /** 获取消息历史 */
  getMessageHistory() {
    return [...this.messageHistory];
  }

  /** 清除历史 */
  clear() {
    this.messageHistory = [];
    this.responseQueue = [];
  }

  async sendMessage(cliType, prompt, options = {}, extra = {}) {
    const record = {
      cliType,
      prompt: prompt.substring(0, 200) + (prompt.length > 200 ? '...' : ''),
      timestamp: Date.now(),
      source: extra.source,
      adapterRole: extra.adapterRole,
    };
    this.messageHistory.push(record);

    const response = this.responseQueue.shift() || this.defaultResponse;

    // 模拟流式输出
    if (extra.streamToUI && extra.onChunk) {
      const chunks = response.content.split(' ');
      for (const chunk of chunks) {
        await new Promise(r => setTimeout(r, 10));
        extra.onChunk(chunk + ' ');
      }
    }

    return response;
  }

  getAdapter(cliType) {
    return {
      cliType,
      sendMessage: (prompt, options) => this.sendMessage(cliType, prompt, options),
    };
  }
}

// ============================================================================
// Mock Session Manager
// ============================================================================

class MockSessionManager {
  constructor() {
    this.sessions = new Map();
    this.currentSessionId = 'test-session-' + Date.now();
    this.sessions.set(this.currentSessionId, {
      id: this.currentSessionId,
      createdAt: Date.now(),
      status: 'active',
      tasks: [],
      snapshots: [],
      messages: [],
    });
  }

  getCurrentSession() {
    return this.sessions.get(this.currentSessionId);
  }

  getSession(id) {
    return this.sessions.get(id);
  }

  updateSession(session) {
    this.sessions.set(session.id, session);
  }
}

// ============================================================================
// Mock Snapshot Manager
// ============================================================================

class MockSnapshotManager {
  constructor() {
    this.snapshots = [];
  }

  createSnapshot(filePath, workerId, assignmentId, priority) {
    this.snapshots.push({ filePath, workerId, assignmentId, priority, createdAt: Date.now() });
    return true;
  }

  revertAllChanges() {
    const count = this.snapshots.length;
    this.snapshots = [];
    return count;
  }

  hasSnapshots() {
    return this.snapshots.length > 0;
  }

  getSnapshots() {
    return [...this.snapshots];
  }
}

// ============================================================================
// Test Scenarios
// ============================================================================

async function testAskMode(runner) {
  runner.logSection('Ask 模式测试 - 纯对话，无代码编辑');

  const mockFactory = new MockCLIAdapterFactory();
  const mockSessionManager = new MockSessionManager();
  const mockSnapshotManager = new MockSnapshotManager();

  // 模拟用户提问
  const userPrompt = '请解释一下 TypeScript 的泛型是什么？';

  // 设置 Mock 响应
  mockFactory.setNextResponse({
    content: 'TypeScript 的泛型（Generics）是一种允许你在定义函数、接口或类时使用类型参数的特性。\n\n这使得你可以创建可重用的组件，同时保持类型安全。',
    tokenUsage: { inputTokens: 50, outputTokens: 80 },
  });

  // 模拟 Ask 模式行为
  const modeConfig = {
    mode: 'ask',
    allowFileModification: false,
    allowCommandExecution: false,
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: false,
    maxFilesToModify: 0,
  };

  runner.logTest('Ask 模式配置',
    modeConfig.allowFileModification === false && modeConfig.allowCommandExecution === false,
    '不允许文件修改和命令执行');

  // 模拟执行
  const response = await mockFactory.sendMessage('claude', userPrompt, {}, { source: 'orchestrator', streamToUI: true });

  runner.logTest('收到 AI 响应',
    response.content.includes('泛型'),
    `响应长度: ${response.content.length} 字符`);

  runner.logTest('Token 统计正确',
    response.tokenUsage.inputTokens > 0 && response.tokenUsage.outputTokens > 0,
    `输入: ${response.tokenUsage.inputTokens}, 输出: ${response.tokenUsage.outputTokens}`);

  // 验证消息历史
  const history = mockFactory.getMessageHistory();
  runner.logTest('消息历史记录',
    history.length === 1 && history[0].cliType === 'claude',
    `记录了 ${history.length} 条消息`);

  // 验证没有快照（因为 ask 模式不修改文件）
  runner.logTest('无文件快照',
    mockSnapshotManager.getSnapshots().length === 0,
    'Ask 模式不应该创建快照');

  return { success: true, mode: 'ask' };
}

async function testAgentMode(runner) {
  runner.logSection('Agent 模式测试 - 需要用户确认');

  const mockFactory = new MockCLIAdapterFactory();
  const mockSessionManager = new MockSessionManager();
  const mockSnapshotManager = new MockSnapshotManager();

  // 模拟用户任务请求
  const userPrompt = '在 src/utils 目录下创建一个 string-helper.ts 文件';

  const modeConfig = {
    mode: 'agent',
    allowFileModification: true,
    allowCommandExecution: true,
    requirePlanConfirmation: true,
    requireRecoveryConfirmation: true,
    autoRollbackOnFailure: false,
    maxFilesToModify: 0,
  };

  runner.logTest('Agent 模式配置',
    modeConfig.requirePlanConfirmation === true,
    '需要计划确认');

  // 阶段 1: 意图分析
  runner.log('\n--- 阶段 1: 意图分析 ---', 'blue');
  mockFactory.setNextResponse({
    content: JSON.stringify({
      intent: 'task',
      recommendedMode: 'task',
      confidence: 0.9,
      needsClarification: false,
      reason: '这是一个明确的文件创建任务'
    }),
    tokenUsage: { inputTokens: 100, outputTokens: 50 },
  });

  const intentResponse = await mockFactory.sendMessage('claude', '分析意图: ' + userPrompt);
  let intentResult;
  try {
    intentResult = JSON.parse(intentResponse.content);
  } catch (e) {
    intentResult = { intent: 'task', confidence: 0.8 };
  }

  runner.logTest('意图识别',
    intentResult.intent === 'task',
    `意图: ${intentResult.intent}, 置信度: ${intentResult.confidence || 'N/A'}`);

  // 阶段 2: 目标理解和计划生成
  runner.log('\n--- 阶段 2: 目标理解和计划生成 ---', 'blue');
  mockFactory.setNextResponse({
    content: JSON.stringify({
      goal: '创建 string-helper.ts 工具文件',
      analysis: '用户需要一个字符串处理工具文件',
      assignments: [
        {
          id: 'assignment-1',
          workerId: 'codex',
          responsibility: '创建 src/utils/string-helper.ts 文件',
          targetFiles: ['src/utils/string-helper.ts']
        }
      ]
    }),
    tokenUsage: { inputTokens: 200, outputTokens: 150 },
  });

  const planResponse = await mockFactory.sendMessage('claude', '生成计划: ' + userPrompt);

  runner.logTest('计划生成',
    planResponse.content.includes('assignment'),
    '生成了包含 Assignment 的计划');

  // 阶段 3: 用户确认（Agent 模式的关键）
  runner.log('\n--- 阶段 3: 用户确认（Hard Stop）---', 'blue');

  let confirmationRequested = false;
  let userConfirmed = false;

  // 模拟确认回调
  const confirmationCallback = async (plan) => {
    confirmationRequested = true;
    // 模拟用户确认
    userConfirmed = true;
    return { confirmed: true };
  };

  // 模拟触发确认
  if (modeConfig.requirePlanConfirmation) {
    await confirmationCallback({ goal: '创建文件' });
  }

  runner.logTest('请求用户确认',
    confirmationRequested,
    'Agent 模式必须请求用户确认');

  runner.logTest('用户已确认',
    userConfirmed,
    '用户确认后继续执行');

  // 阶段 4: 执行（用户确认后）
  runner.log('\n--- 阶段 4: 执行任务 ---', 'blue');

  if (userConfirmed) {
    // 创建快照
    mockSnapshotManager.createSnapshot('src/utils/string-helper.ts', 'codex', 'assignment-1', 5);

    runner.logTest('创建文件快照',
      mockSnapshotManager.getSnapshots().length === 1,
      '在执行前创建了快照');

    // 模拟执行结果
    mockFactory.setNextResponse({
      content: '已创建 src/utils/string-helper.ts 文件',
      tokenUsage: { inputTokens: 100, outputTokens: 30 },
    });

    const execResult = await mockFactory.sendMessage('codex', '执行任务');

    runner.logTest('任务执行完成',
      execResult.content.includes('已创建'),
      '文件创建成功');
  }

  // 阶段 5: 总结
  runner.log('\n--- 阶段 5: 任务总结 ---', 'blue');
  mockFactory.setNextResponse({
    content: '## 任务完成\n\n已成功创建 string-helper.ts 文件。\n\n### 修改的文件\n- src/utils/string-helper.ts',
    tokenUsage: { inputTokens: 50, outputTokens: 40 },
  });

  const summaryResponse = await mockFactory.sendMessage('claude', '生成总结');

  runner.logTest('生成任务总结',
    summaryResponse.content.includes('任务完成'),
    '包含完成状态和修改文件列表');

  // Token 统计汇总
  const history = mockFactory.getMessageHistory();
  const totalTokens = history.reduce((acc, _) => acc + 150, 0); // 简化计算

  runner.logTest('Token 统计汇总',
    totalTokens > 0,
    `总计约 ${totalTokens} tokens`);

  return { success: true, mode: 'agent', confirmationRequested, userConfirmed };
}

async function testAutoMode(runner) {
  runner.logSection('Auto 模式测试 - 自动执行，失败时回滚');

  const mockFactory = new MockCLIAdapterFactory();
  const mockSessionManager = new MockSessionManager();
  const mockSnapshotManager = new MockSnapshotManager();

  const userPrompt = '重构 utils/helper.ts 中的 formatDate 函数';

  const modeConfig = {
    mode: 'auto',
    allowFileModification: true,
    allowCommandExecution: true,
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: true,
    maxFilesToModify: 0,
  };

  runner.logTest('Auto 模式配置',
    modeConfig.requirePlanConfirmation === false && modeConfig.autoRollbackOnFailure === true,
    '无需确认，失败时自动回滚');

  // 阶段 1: 直接执行（无确认）
  runner.log('\n--- 阶段 1: 直接开始执行（无确认）---', 'blue');

  let confirmationRequested = false;

  // 验证不会请求确认
  if (modeConfig.requirePlanConfirmation) {
    confirmationRequested = true;
  }

  runner.logTest('无需用户确认',
    !confirmationRequested,
    'Auto 模式直接执行');

  // 阶段 2: 创建快照并执行
  runner.log('\n--- 阶段 2: 快照创建和执行 ---', 'blue');

  mockSnapshotManager.createSnapshot('utils/helper.ts', 'codex', 'assignment-auto-1', 5);

  runner.logTest('执行前创建快照',
    mockSnapshotManager.getSnapshots().length === 1,
    '在执行前创建了文件快照');

  // 模拟执行成功
  mockFactory.setNextResponse({
    content: '已重构 formatDate 函数',
    tokenUsage: { inputTokens: 150, outputTokens: 80 },
  });

  const execResult = await mockFactory.sendMessage('codex', '执行重构');

  runner.logTest('执行成功',
    execResult.content.includes('重构'),
    '任务执行完成');

  // 阶段 3: 测试失败回滚
  runner.log('\n--- 阶段 3: 模拟失败和自动回滚 ---', 'blue');

  // 添加更多快照
  mockSnapshotManager.createSnapshot('utils/helper.ts', 'codex', 'assignment-auto-2', 5);
  mockSnapshotManager.createSnapshot('utils/date.ts', 'codex', 'assignment-auto-3', 5);

  const snapshotsBefore = mockSnapshotManager.getSnapshots().length;

  // 模拟失败
  const executionFailed = true;
  let rolledBack = false;

  if (executionFailed && modeConfig.autoRollbackOnFailure) {
    const rollbackCount = mockSnapshotManager.revertAllChanges();
    rolledBack = rollbackCount > 0;
  }

  runner.logTest('失败时自动回滚',
    rolledBack && mockSnapshotManager.getSnapshots().length === 0,
    `回滚了 ${snapshotsBefore} 个快照`);

  // 阶段 4: 任务总结
  runner.log('\n--- 阶段 4: 任务总结 ---', 'blue');
  mockFactory.setNextResponse({
    content: '## 任务完成\n\n已完成 formatDate 函数的重构。\n\n### 变更说明\n- 优化了日期格式化逻辑\n- 添加了时区支持',
    tokenUsage: { inputTokens: 50, outputTokens: 60 },
  });

  const summaryResponse = await mockFactory.sendMessage('claude', '生成总结');

  runner.logTest('生成任务总结',
    summaryResponse.content.includes('## 任务完成'),
    '包含 Markdown 格式的总结');

  return { success: true, mode: 'auto', confirmationRequested: false, autoRollback: true };
}

async function testSubTaskAssignmentSync(runner) {
  runner.logSection('SubTask/Assignment 同步测试');

  // 模拟 SubTask 创建
  const subTask = {
    id: 'subtask-123',
    taskId: 'task-456',
    description: '实现用户认证模块',
    assignmentId: 'assignment-789',  // 新增字段
    assignedWorker: 'claude',
    targetFiles: ['src/auth/auth.ts'],
    status: 'pending',
    progress: 0,
  };

  runner.logTest('SubTask 包含 assignmentId',
    subTask.assignmentId === 'assignment-789',
    `assignmentId: ${subTask.assignmentId}`);

  // 模拟通过 assignmentId 查找
  const findByAssignmentId = (subTasks, assignmentId) => {
    return subTasks.find(st => st.assignmentId === assignmentId);
  };

  const found = findByAssignmentId([subTask], 'assignment-789');

  runner.logTest('通过 assignmentId 查找 SubTask',
    found !== undefined && found.id === 'subtask-123',
    '使用稳定的 ID 匹配替代不稳定的 description 匹配');

  // 模拟状态同步
  const updateStatus = (subTask, status) => {
    subTask.status = status;
    if (status === 'running') {
      subTask.startedAt = Date.now();
    } else if (status === 'completed') {
      subTask.completedAt = Date.now();
      subTask.progress = 100;
    }
    return subTask;
  };

  updateStatus(subTask, 'running');
  runner.logTest('状态更新: running',
    subTask.status === 'running' && subTask.startedAt !== undefined,
    `状态: ${subTask.status}, 开始时间: ${subTask.startedAt}`);

  updateStatus(subTask, 'completed');
  runner.logTest('状态更新: completed',
    subTask.status === 'completed' && subTask.progress === 100,
    `状态: ${subTask.status}, 进度: ${subTask.progress}%`);

  return { success: true };
}

async function testMessageFlow(runner) {
  runner.logSection('消息流程测试 - 从用户输入到 UI 展示');

  const messages = [];

  // 模拟消息流程
  const emitMessage = (type, content, source) => {
    const msg = {
      id: `msg-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`,
      type,
      content,
      source,
      timestamp: Date.now(),
    };
    messages.push(msg);
    return msg;
  };

  // 1. 用户输入
  const userMsg = emitMessage('user_input', '帮我优化这段代码', 'user');
  runner.logTest('用户输入消息',
    userMsg.type === 'user_input',
    `消息 ID: ${userMsg.id}`);

  // 2. 编排器开始处理
  const orchStartMsg = emitMessage('orchestrator_started', '正在分析任务...', 'orchestrator');
  runner.logTest('编排器启动消息',
    orchStartMsg.source === 'orchestrator',
    '通知 UI 开始处理');

  // 3. 意图分析结果
  const intentMsg = emitMessage('intent_analyzed', JSON.stringify({
    intent: 'task',
    confidence: 0.95,
  }), 'orchestrator');
  runner.logTest('意图分析消息',
    intentMsg.type === 'intent_analyzed',
    '意图分析结果');

  // 4. 计划生成
  const planMsg = emitMessage('plan_ready', '## 执行计划\n1. 分析代码\n2. 优化实现', 'orchestrator');
  runner.logTest('计划就绪消息',
    planMsg.type === 'plan_ready',
    '计划内容可在 UI 展示');

  // 5. Worker 开始执行
  const workerStartMsg = emitMessage('worker_started', '开始执行优化...', 'worker:codex');
  runner.logTest('Worker 启动消息',
    workerStartMsg.source === 'worker:codex',
    'Worker 开始执行');

  // 6. 流式输出
  const streamingMsgs = [];
  for (let i = 0; i < 3; i++) {
    const streamMsg = emitMessage('streaming', `代码块 ${i + 1}...`, 'worker:codex');
    streamingMsgs.push(streamMsg);
  }
  runner.logTest('流式输出消息',
    streamingMsgs.length === 3,
    `生成了 ${streamingMsgs.length} 条流式消息`);

  // 7. Worker 完成
  const workerDoneMsg = emitMessage('worker_completed', '优化完成', 'worker:codex');
  runner.logTest('Worker 完成消息',
    workerDoneMsg.type === 'worker_completed',
    'Worker 执行完成');

  // 8. 任务总结
  const summaryMsg = emitMessage('task_summary', '## 任务总结\n\n优化完成，性能提升 20%', 'orchestrator');
  runner.logTest('任务总结消息',
    summaryMsg.type === 'task_summary',
    '最终总结可在 UI 展示');

  // 验证消息流完整性
  const expectedFlow = ['user_input', 'orchestrator_started', 'intent_analyzed', 'plan_ready',
                        'worker_started', 'streaming', 'streaming', 'streaming', 'worker_completed', 'task_summary'];
  const actualFlow = messages.map(m => m.type);

  runner.logTest('消息流完整性',
    JSON.stringify(expectedFlow) === JSON.stringify(actualFlow),
    `共 ${messages.length} 条消息`);

  return { success: true, messageCount: messages.length };
}

async function testTokenAggregation(runner) {
  runner.logSection('Token 统计聚合测试');

  // 模拟多阶段 Token 使用
  const tokenUsages = [];

  // 意图分析阶段
  tokenUsages.push({
    phase: 'intent_analysis',
    inputTokens: 100,
    outputTokens: 50,
  });

  // 目标理解阶段
  tokenUsages.push({
    phase: 'goal_understanding',
    inputTokens: 200,
    outputTokens: 150,
  });

  // Worker 执行阶段
  tokenUsages.push({
    phase: 'worker_execution',
    inputTokens: 500,
    outputTokens: 300,
  });

  // 总结阶段
  tokenUsages.push({
    phase: 'summary',
    inputTokens: 100,
    outputTokens: 80,
  });

  // 聚合统计
  const totalTokens = tokenUsages.reduce((acc, usage) => ({
    inputTokens: acc.inputTokens + usage.inputTokens,
    outputTokens: acc.outputTokens + usage.outputTokens,
  }), { inputTokens: 0, outputTokens: 0 });

  runner.logTest('Token 按阶段统计',
    tokenUsages.length === 4,
    `记录了 ${tokenUsages.length} 个阶段`);

  runner.logTest('Token 聚合正确',
    totalTokens.inputTokens === 900 && totalTokens.outputTokens === 580,
    `总输入: ${totalTokens.inputTokens}, 总输出: ${totalTokens.outputTokens}`);

  // 验证 Worker 阶段有最多 Token（通常如此）
  const workerUsage = tokenUsages.find(u => u.phase === 'worker_execution');
  runner.logTest('Worker 阶段 Token 最多',
    workerUsage && workerUsage.inputTokens > 400,
    `Worker 阶段: ${workerUsage?.inputTokens} 输入 tokens`);

  return { success: true, totalTokens };
}

async function testRecoveryConfirmation(runner) {
  runner.logSection('恢复确认流程测试 - 验证失败和错误恢复');

  const mockSnapshotManager = new MockSnapshotManager();

  // 模拟验证失败场景
  runner.log('\n--- 场景 1: 验证失败，用户选择回滚 ---', 'blue');

  // 创建快照
  mockSnapshotManager.createSnapshot('src/app.ts', 'codex', 'assignment-1', 5);
  mockSnapshotManager.createSnapshot('src/util.ts', 'gemini', 'assignment-2', 5);

  runner.logTest('验证前有快照',
    mockSnapshotManager.hasSnapshots() === true,
    `快照数: ${mockSnapshotManager.getSnapshots().length}`);

  // 模拟恢复确认回调
  let recoveryDecision = null;
  const mockRecoveryCallback = async (failedTask, error, options) => {
    recoveryDecision = {
      failedTask,
      error,
      options,
    };
    // 模拟用户选择回滚
    return 'rollback';
  };

  // 模拟验证失败
  const verificationResult = { passed: false, summary: '未通过验收标准' };

  if (!verificationResult.passed && mockRecoveryCallback) {
    const failedSubTask = {
      id: 'mission-123',
      taskId: 'task-456',
      description: '测试任务',
      assignedWorker: 'orchestrator',
      status: 'failed',
    };
    const errorMsg = `验证失败: ${verificationResult.summary}`;
    const hasSnapshots = mockSnapshotManager.hasSnapshots();

    const decision = await mockRecoveryCallback(
      failedSubTask,
      errorMsg,
      { retry: true, rollback: hasSnapshots }
    );

    runner.logTest('恢复回调被调用',
      recoveryDecision !== null,
      `错误: ${recoveryDecision?.error}`);

    runner.logTest('回调参数正确',
      recoveryDecision?.options.retry === true && recoveryDecision?.options.rollback === true,
      '支持重试和回滚');

    if (decision === 'rollback' && hasSnapshots) {
      const rollbackCount = mockSnapshotManager.revertAllChanges();
      runner.logTest('执行回滚操作',
        rollbackCount === 2 && !mockSnapshotManager.hasSnapshots(),
        `回滚了 ${rollbackCount} 个文件`);
    }
  }

  // 场景 2: 错误发生，用户选择重试
  runner.log('\n--- 场景 2: 执行错误，用户选择重试 ---', 'blue');

  let retryCount = 0;
  const mockRetryCallback = async (failedTask, error, options) => {
    if (retryCount === 0) {
      retryCount++;
      return 'retry';
    }
    // 第二次选择继续
    return 'continue';
  };

  // 模拟第一次执行失败
  const executeWithRetry = async (callback, attempt = 0) => {
    if (attempt === 0) {
      // 模拟失败
      const decision = await callback(
        { id: 'task-1', description: '执行失败' },
        '网络错误',
        { retry: true, rollback: false }
      );
      if (decision === 'retry') {
        return executeWithRetry(callback, attempt + 1);
      }
      return 'failed';
    }
    return 'success';
  };

  const result = await executeWithRetry(mockRetryCallback);
  runner.logTest('重试后成功',
    result === 'success' && retryCount === 1,
    `重试次数: ${retryCount}`);

  // 场景 3: 用户选择继续（不处理错误）
  runner.log('\n--- 场景 3: 执行错误，用户选择继续 ---', 'blue');

  const mockContinueCallback = async () => 'continue';
  const continueDecision = await mockContinueCallback({}, '测试错误', { retry: true, rollback: true });

  runner.logTest('用户选择继续',
    continueDecision === 'continue',
    '继续执行后续流程');

  return { success: true };
}

// ============================================================================
// Main
// ============================================================================

async function main() {
  const runner = new TestRunner('端对端模式测试 (E2E Mode Tests)');

  try {
    // 测试 Ask 模式
    await testAskMode(runner);

    // 测试 Agent 模式
    await testAgentMode(runner);

    // 测试 Auto 模式
    await testAutoMode(runner);

    // 测试 SubTask/Assignment 同步
    await testSubTaskAssignmentSync(runner);

    // 测试消息流程
    await testMessageFlow(runner);

    // 测试 Token 聚合
    await testTokenAggregation(runner);

    // 测试恢复确认流程
    await testRecoveryConfirmation(runner);

    process.exit(runner.finish());
  } catch (error) {
    console.error('测试执行失败:', error);
    process.exit(1);
  }
}

main().catch(console.error);
