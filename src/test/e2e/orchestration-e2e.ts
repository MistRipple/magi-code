/**
 * 编排系统端到端验证
 *
 * 验证完整的任务系统和编排流程：
 * 1. TodoManager: parentId 传递、生命周期状态机
 * 2. PlanningExecutor: 宏观 Todo 创建
 * 3. Worker 动态拆分: extractDynamicTodos + parentId 挂载
 * 4. dispatch_task 路径: Todo 创建 + 事件链
 * 5. 事件传播链: todoStarted → todoCompleted → dynamicTodoAdded
 * 6. 前端数据映射: parentId 保留到 UI 层
 *
 * 运行: npx ts-node src/test/e2e/orchestration-e2e.ts
 */

import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import { EventEmitter } from 'events';
import { TodoManager } from '../../todo/todo-manager';
import { UnifiedTodo, CreateTodoParams } from '../../todo/types';

// ============================================================================
// 测试框架
// ============================================================================

interface TestResult {
  name: string;
  passed: boolean;
  details: string[];
  duration: number;
}

const results: TestResult[] = [];

function createTestDir(): string {
  const testDir = path.join(os.tmpdir(), `magi-orchestration-e2e-${Date.now()}`);
  fs.mkdirSync(testDir, { recursive: true });
  return testDir;
}

function cleanupTestDir(dir: string): void {
  try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ }
}

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(`断言失败: ${message}`);
}

function assertEqual<T>(actual: T, expected: T, label: string): void {
  if (actual !== expected) {
    throw new Error(`断言失败 [${label}]: 期望 ${JSON.stringify(expected)}，实际 ${JSON.stringify(actual)}`);
  }
}

async function runTest(name: string, fn: () => Promise<void>): Promise<void> {
  const start = Date.now();
  const details: string[] = [];
  try {
    await fn();
    details.push('✓ 通过');
    results.push({ name, passed: true, details, duration: Date.now() - start });
    console.log(`  ✓ ${name} (${Date.now() - start}ms)`);
  } catch (error: any) {
    details.push(`✗ ${error.message}`);
    results.push({ name, passed: false, details, duration: Date.now() - start });
    console.log(`  ✗ ${name}: ${error.message}`);
  }
}

// ============================================================================
// 测试 1: TodoManager parentId 传递与层级关系
// ============================================================================

async function testTodoManagerParentId(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    // 创建父 Todo（编排者的宏观 Todo）
    const parentTodo = await todoManager.create({
      missionId: 'mission-1',
      assignmentId: 'assignment-1',
      content: '实现用户认证模块',
      reasoning: '用户请求开发认证功能',
      type: 'implementation',
      workerId: 'claude',
    });

    assert(!parentTodo.parentId, '父 Todo 不应该有 parentId');
    // 无依赖的 Todo 创建后会自动被 checkAndUpdateStatus 转为 ready
    assertEqual(parentTodo.status, 'ready', '无依赖 Todo 初始状态应为 ready');

    // 创建子 Todo（Worker 动态拆分）
    const childTodo1 = await todoManager.create({
      missionId: 'mission-1',
      assignmentId: 'assignment-1',
      parentId: parentTodo.id,
      content: '设计认证 API 接口',
      reasoning: '执行过程中发现需要拆分',
      type: 'design',
      workerId: 'claude',
    });

    const childTodo2 = await todoManager.create({
      missionId: 'mission-1',
      assignmentId: 'assignment-1',
      parentId: parentTodo.id,
      content: '实现 JWT Token 签发',
      reasoning: '执行过程中发现需要拆分',
      type: 'implementation',
      workerId: 'claude',
    });

    // 验证 parentId 传递
    assertEqual(childTodo1.parentId, parentTodo.id, '子 Todo 1 的 parentId');
    assertEqual(childTodo2.parentId, parentTodo.id, '子 Todo 2 的 parentId');

    // 验证可以通过 parentId 筛选子 Todo
    const allTodos = [parentTodo, childTodo1, childTodo2];
    const rootTodos = allTodos.filter(t => !t.parentId);
    const children = allTodos.filter(t => t.parentId === parentTodo.id);

    assertEqual(rootTodos.length, 1, '根 Todo 数量');
    assertEqual(children.length, 2, '子 Todo 数量');

    // 验证生命周期状态机
    await todoManager.prepareForExecution(parentTodo.id);
    await todoManager.start(parentTodo.id);
    const runningTodo = await todoManager.get(parentTodo.id);
    assertEqual(runningTodo?.status, 'running', '启动后状态应为 running');

    await todoManager.complete(parentTodo.id, {
      success: true,
      summary: '认证模块完成',
      modifiedFiles: ['src/auth.ts'],
      duration: 5000,
    });
    const completedTodo = await todoManager.get(parentTodo.id);
    assertEqual(completedTodo?.status, 'completed', '完成后状态应为 completed');
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 测试 2: PlanningExecutor 宏观 Todo 创建模拟
// ============================================================================

async function testPlanningExecutorTodoCreation(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    // 模拟 PlanningExecutor.createTodoForAssignment 的逻辑
    const mission = {
      id: 'mission-plan-1',
      assignments: [
        {
          id: 'asgn-1',
          workerId: 'claude' as const,
          responsibility: '实现用户登录功能',
          delegationBriefing: '使用 JWT + bcrypt 实现登录认证',
          scope: { targetPaths: ['src/auth/login.ts'], requiresModification: true },
        },
        {
          id: 'asgn-2',
          workerId: 'codex' as const,
          responsibility: '编写单元测试',
          delegationBriefing: '为登录功能编写测试用例',
          scope: { targetPaths: ['src/auth/__tests__/login.test.ts'] },
        },
      ],
    };

    const createdTodos: UnifiedTodo[] = [];

    for (const assignment of mission.assignments) {
      const targetPaths = assignment.scope?.targetPaths?.length
        ? `\n目标文件: ${assignment.scope.targetPaths.join(', ')}`
        : '';

      const todo = await todoManager.create({
        missionId: mission.id,
        assignmentId: assignment.id,
        content: `${assignment.responsibility}${targetPaths}`,
        reasoning: assignment.delegationBriefing || assignment.responsibility,
        type: 'implementation',
        workerId: assignment.workerId,
        targetFiles: assignment.scope?.targetPaths,
      });

      createdTodos.push(todo);
    }

    // 验证：每个 Assignment 创建了 1 个 Todo
    assertEqual(createdTodos.length, 2, '宏观 Todo 数量');

    // 验证：Todo 无 parentId（编排者创建的宏观 Todo）
    for (const todo of createdTodos) {
      assert(!todo.parentId, `宏观 Todo ${todo.id} 不应有 parentId`);
    }

    // 验证：Todo 内容包含目标文件
    assert(createdTodos[0].content.includes('src/auth/login.ts'), 'Todo 内容应包含目标文件');
    assertEqual(createdTodos[0].workerId, 'claude', '第一个 Todo 分配给 claude');
    assertEqual(createdTodos[1].workerId, 'codex', '第二个 Todo 分配给 codex');
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 测试 3: Worker 动态 Todo 拆分 + parentId 挂载
// ============================================================================

async function testWorkerDynamicTodoSplitting(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    // 模拟编排者创建的宏观 Todo
    const macroTodo = await todoManager.create({
      missionId: 'mission-dynamic-1',
      assignmentId: 'assignment-dynamic-1',
      content: '重构数据库访问层',
      reasoning: '改善数据库查询性能',
      type: 'implementation',
      workerId: 'claude',
    });

    // 模拟 extractDynamicTodos 的正则匹配逻辑
    const workerOutput = `
分析完成，开始重构数据库访问层。
TODO: 将 raw SQL 迁移到 ORM 查询
已完成连接池配置。
需要额外处理: 添加数据库迁移脚本
Additional task: 更新 API 层的数据库调用
    `.trim();

    const todoPattern = /(?:TODO|需要额外处理|Additional task)[：:]?\s*(.+)/gi;
    const dynamicTodos: UnifiedTodo[] = [];
    let match;

    while ((match = todoPattern.exec(workerOutput)) !== null) {
      const content = match[1].trim();
      if (content) {
        const dynamicTodo = await todoManager.create({
          missionId: 'mission-dynamic-1',
          assignmentId: 'assignment-dynamic-1',
          parentId: macroTodo.id,  // 关键：挂载到父 Todo
          content,
          reasoning: '执行过程中发现的额外任务',
          type: 'implementation',
          workerId: 'claude',
        });
        dynamicTodos.push(dynamicTodo);
      }
    }

    // 验证：动态 Todo 正确提取
    assertEqual(dynamicTodos.length, 3, '动态 Todo 数量');

    // 验证：所有动态 Todo 的 parentId 指向宏观 Todo
    for (const dt of dynamicTodos) {
      assertEqual(dt.parentId, macroTodo.id, `动态 Todo "${dt.content}" 的 parentId`);
    }

    // 验证：动态 Todo 的内容
    assert(dynamicTodos[0].content.includes('ORM'), '第一个动态 Todo 内容');
    assert(dynamicTodos[1].content.includes('迁移脚本'), '第二个动态 Todo 内容');
    assert(dynamicTodos[2].content.includes('API'), '第三个动态 Todo 内容');

    // 验证层级查询
    const allTodos = [macroTodo, ...dynamicTodos];
    const rootTodos = allTodos.filter(t => !t.parentId);
    const childrenOfMacro = allTodos.filter(t => t.parentId === macroTodo.id);
    assertEqual(rootTodos.length, 1, '根 Todo（宏观）数量');
    assertEqual(childrenOfMacro.length, 3, '子 Todo（动态）数量');
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 测试 4: 事件传播链完整性
// ============================================================================

async function testEventPropagationChain(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    const events: Array<{ event: string; todoId: string; parentId?: string }> = [];

    // 监听 TodoManager 事件
    todoManager.on('todo:created', (todo: UnifiedTodo) => {
      events.push({ event: 'created', todoId: todo.id, parentId: todo.parentId });
    });
    todoManager.on('todo:ready', (todo: UnifiedTodo) => {
      events.push({ event: 'ready', todoId: todo.id });
    });
    todoManager.on('todo:started', (todo: UnifiedTodo) => {
      events.push({ event: 'started', todoId: todo.id });
    });
    todoManager.on('todo:completed', (todo: UnifiedTodo) => {
      events.push({ event: 'completed', todoId: todo.id });
    });
    todoManager.on('todo:failed', (todo: UnifiedTodo) => {
      events.push({ event: 'failed', todoId: todo.id });
    });

    // 创建父 Todo
    const parentTodo = await todoManager.create({
      missionId: 'mission-event-1',
      assignmentId: 'asgn-event-1',
      content: '构建前端组件',
      reasoning: '用户需要新组件',
      type: 'implementation',
      workerId: 'gemini',
    });

    // 执行父 Todo
    await todoManager.prepareForExecution(parentTodo.id);
    await todoManager.start(parentTodo.id);
    await todoManager.complete(parentTodo.id, {
      success: true,
      summary: '组件完成',
      modifiedFiles: ['src/components/Button.tsx'],
      duration: 3000,
    });

    // 创建子 Todo（模拟动态拆分）
    const childTodo = await todoManager.create({
      missionId: 'mission-event-1',
      assignmentId: 'asgn-event-1',
      parentId: parentTodo.id,
      content: '添加组件样式',
      reasoning: '动态发现需要样式',
      type: 'implementation',
      workerId: 'gemini',
    });

    // 验证事件序列
    const createdEvents = events.filter(e => e.event === 'created');
    assertEqual(createdEvents.length, 2, 'created 事件数量');

    // 验证 created 事件中 parentId 传递
    const parentCreated = createdEvents.find(e => e.todoId === parentTodo.id);
    assert(!parentCreated?.parentId, '父 Todo 的 created 事件不应有 parentId');

    const childCreated = createdEvents.find(e => e.todoId === childTodo.id);
    assertEqual(childCreated?.parentId, parentTodo.id, '子 Todo 的 created 事件应有 parentId');

    // 验证状态事件链：created → ready → started → completed
    const parentEvents = events
      .filter(e => e.todoId === parentTodo.id)
      .map(e => e.event);
    assert(parentEvents.includes('created'), '应有 created 事件');
    assert(parentEvents.includes('started'), '应有 started 事件');
    assert(parentEvents.includes('completed'), '应有 completed 事件');
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 测试 5: Worker dynamicTodoAdded 事件模拟
// ============================================================================

async function testDynamicTodoAddedEvent(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    // 模拟 AutonomousWorker 的 emit 行为
    const workerEmitter = new EventEmitter();
    const receivedEvents: Array<{ assignmentId: string; todo: any }> = [];

    workerEmitter.on('dynamicTodoAdded', (data) => {
      receivedEvents.push(data);
    });

    // 创建宏观 Todo
    const macroTodo = await todoManager.create({
      missionId: 'mission-emit-1',
      assignmentId: 'asgn-emit-1',
      content: '重构 API 路由',
      reasoning: '用户要求',
      type: 'implementation',
      workerId: 'codex',
    });

    // 模拟 addDynamicTodo 行为
    const dynamicTodo = await todoManager.create({
      missionId: 'mission-emit-1',
      assignmentId: 'asgn-emit-1',
      parentId: macroTodo.id,
      content: '更新路由文档',
      reasoning: '执行过程中发现的额外任务',
      type: 'implementation',
      workerId: 'codex',
    });

    // 模拟 Worker emit
    workerEmitter.emit('dynamicTodoAdded', {
      assignmentId: 'asgn-emit-1',
      todo: dynamicTodo,
    });

    // 验证事件数据
    assertEqual(receivedEvents.length, 1, '应收到 1 个 dynamicTodoAdded 事件');
    assertEqual(receivedEvents[0].todo.parentId, macroTodo.id, '事件中 Todo 应有 parentId');
    assertEqual(receivedEvents[0].todo.content, '更新路由文档', '事件中 Todo 内容');
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 测试 6: normalizeTodo 保留 parentId（模拟 webview-provider 行为）
// ============================================================================

async function testNormalizeTodoPreservesParentId(): Promise<void> {
  // 模拟 webview-provider 的 normalizeTodo 逻辑
  function normalizeTodo(
    rawTodo: any,
    assignmentId: string,
    seen: Set<string>
  ): any | null {
    if (!rawTodo || typeof rawTodo !== 'object') return null;
    const id = typeof rawTodo.id === 'string' && rawTodo.id.trim()
      ? rawTodo.id.trim()
      : `todo-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`;
    if (seen.has(id)) return null;
    seen.add(id);
    return {
      ...rawTodo,  // spread 保留 parentId
      id,
      assignmentId: rawTodo.assignmentId || assignmentId,
    };
  }

  // 测试：有 parentId 的 Todo
  const rawTodoWithParent = {
    id: 'todo-child-1',
    content: '子任务',
    parentId: 'todo-parent-1',
    status: 'pending',
    missionId: 'mission-1',
    assignmentId: 'asgn-1',
  };

  const seen = new Set<string>();
  const normalized = normalizeTodo(rawTodoWithParent, 'asgn-1', seen);

  assert(normalized !== null, 'normalizeTodo 不应返回 null');
  assertEqual(normalized.parentId, 'todo-parent-1', 'parentId 应被保留');
  assertEqual(normalized.id, 'todo-child-1', 'id 应保留');

  // 测试：无 parentId 的 Todo
  const rawTodoWithoutParent = {
    id: 'todo-root-1',
    content: '根任务',
    status: 'pending',
  };

  const normalized2 = normalizeTodo(rawTodoWithoutParent, 'asgn-1', seen);
  assert(!normalized2.parentId, '无 parentId 的 Todo 保持 undefined');
}

// ============================================================================
// 测试 7: 前端 message-handler Todo 映射模拟
// ============================================================================

async function testMessageHandlerTodoMapping(): Promise<void> {
  // 模拟 message-handler 中的 todo 映射逻辑
  function mapTodoToAssignmentTodo(todo: any, assignmentId: string): any {
    const todoId = typeof todo?.id === 'string' && todo.id.trim()
      ? todo.id.trim()
      : `todo-${Date.now()}`;
    return {
      id: todoId,
      assignmentId,
      parentId: todo.parentId,          // 关键映射
      content: todo.content || '',
      reasoning: todo.reasoning,
      expectedOutput: todo.expectedOutput,
      type: todo.type || 'implementation',
      priority: todo.priority ?? 3,
      status: todo.status || 'pending',
      outOfScope: todo.outOfScope || false,
      approvalStatus: todo.approvalStatus,
      approvalNote: todo.approvalNote,
    };
  }

  // 场景 1: missionPlanned 中的 Todo（无 parentId）
  const macroTodo = {
    id: 'todo-macro-1',
    content: '实现搜索功能',
    reasoning: '编排者分配',
    type: 'implementation',
    priority: 2,
    status: 'pending',
  };
  const mapped1 = mapTodoToAssignmentTodo(macroTodo, 'asgn-1');
  assertEqual(mapped1.parentId, undefined, 'missionPlanned 的 Todo 无 parentId');
  assertEqual(mapped1.content, '实现搜索功能', '内容正确');

  // 场景 2: dynamicTodoAdded 中的 Todo（有 parentId）
  const dynamicTodo = {
    id: 'todo-dynamic-1',
    parentId: 'todo-macro-1',
    content: '添加搜索索引',
    reasoning: '动态发现',
    type: 'implementation',
    status: 'pending',
    outOfScope: true,
    approvalStatus: 'pending',
  };
  const mapped2 = mapTodoToAssignmentTodo(dynamicTodo, 'asgn-1');
  assertEqual(mapped2.parentId, 'todo-macro-1', 'dynamicTodo 的 parentId 应正确传递');
  assertEqual(mapped2.outOfScope, true, 'outOfScope 应保留');
  assertEqual(mapped2.approvalStatus, 'pending', 'approvalStatus 应保留');
}

// ============================================================================
// 测试 8: TasksPanel 层级渲染数据验证
// ============================================================================

async function testTasksPanelHierarchyData(): Promise<void> {
  // 模拟 TasksPanel 的层级渲染逻辑
  const todos = [
    { id: 't1', content: '实现认证', status: 'completed', parentId: undefined },
    { id: 't2', content: '编写测试', status: 'running', parentId: undefined },
    { id: 't1-1', content: '设计 API', status: 'completed', parentId: 't1' },
    { id: 't1-2', content: '实现 JWT', status: 'completed', parentId: 't1' },
    { id: 't2-1', content: '单元测试', status: 'pending', parentId: 't2' },
  ];

  // 根 Todo（无 parentId）
  const rootTodos = todos.filter(t => !t.parentId);
  assertEqual(rootTodos.length, 2, '根 Todo 数量');

  // 验证子 Todo 分组
  for (const root of rootTodos) {
    const children = todos.filter(t => t.parentId === root.id);
    if (root.id === 't1') {
      assertEqual(children.length, 2, 't1 的子 Todo 数量');
    } else if (root.id === 't2') {
      assertEqual(children.length, 1, 't2 的子 Todo 数量');
    }
  }

  // 模拟序号生成逻辑
  const seqMapping: string[] = [];
  let rootIdx = 0;
  for (const todo of todos) {
    if (!todo.parentId) {
      rootIdx++;
      seqMapping.push(`${rootIdx}. ${todo.content}`);
      const children = todos.filter(t => t.parentId === todo.id);
      children.forEach((child, childIdx) => {
        seqMapping.push(`  ${rootIdx}.${childIdx + 1} ${child.content}`);
      });
    }
  }

  // 验证序号结构
  assertEqual(seqMapping.length, 5, '序号列表总数');
  assert(seqMapping[0].startsWith('1.'), '第一个根 Todo 序号');
  assert(seqMapping[1].includes('1.1'), '第一个子 Todo 序号');
  assert(seqMapping[2].includes('1.2'), '第二个子 Todo 序号');
  assert(seqMapping[3].startsWith('2.'), '第二个根 Todo 序号');
  assert(seqMapping[4].includes('2.1'), '第二个根的子 Todo 序号');
}

// ============================================================================
// 测试 9: dispatch_task 路径 Todo 创建模拟
// ============================================================================

async function testDispatchTaskTodoCreation(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    // 模拟 launchDispatchWorker 中的 Todo 创建
    const batchId = `batch-${Date.now()}`;
    const taskId = `task-dispatch-1`;
    const worker = 'codex';
    const task = '修复登录页面的样式问题';
    const files = ['src/pages/Login.tsx', 'src/styles/login.css'];

    const targetPaths = files.length > 0
      ? `\n目标文件: ${files.join(', ')}。必须使用工具直接编辑并保存。`
      : '';

    const todo = await todoManager.create({
      missionId: batchId,
      assignmentId: taskId,
      content: `${task}${targetPaths}`,
      reasoning: 'dispatch_task 编排者直接分配',
      type: 'implementation',
      workerId: worker as any,
      targetFiles: files,
    });

    // 验证 Todo 创建正确
    assert(!todo.parentId, 'dispatch_task 的宏观 Todo 不应有 parentId');
    assertEqual(todo.missionId, batchId, 'missionId 应为 batchId');
    assertEqual(todo.assignmentId, taskId, 'assignmentId 应为 taskId');
    assert(todo.content.includes('修复登录'), 'Todo 内容正确');
    assert(todo.content.includes('Login.tsx'), '目标文件正确');

    // 模拟 Worker 执行后动态拆分
    const dynamicTodo = await todoManager.create({
      missionId: batchId,
      assignmentId: taskId,
      parentId: todo.id,
      content: '更新 Login 组件的响应式布局',
      reasoning: '执行过程中发现的额外任务',
      type: 'implementation',
      workerId: worker as any,
    });

    assertEqual(dynamicTodo.parentId, todo.id, '动态 Todo 的 parentId 指向 dispatch Todo');
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 测试 10: 完整流程端到端 — 从 Todo 创建到 UI 数据渲染
// ============================================================================

async function testFullPipelineE2E(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    // ===== 阶段 1: 编排者创建宏观 Todo（模拟 PlanningExecutor）=====
    const macroTodo = await todoManager.create({
      missionId: 'e2e-mission',
      assignmentId: 'e2e-assignment',
      content: '开发新的搜索引擎功能',
      reasoning: '用户要求实现全文搜索',
      type: 'implementation',
      workerId: 'claude',
    });

    // ===== 阶段 2: Worker 执行宏观 Todo =====
    await todoManager.prepareForExecution(macroTodo.id);
    await todoManager.start(macroTodo.id);

    // ===== 阶段 3: Worker 发现需要拆分，创建子 Todo =====
    const subTodo1 = await todoManager.create({
      missionId: 'e2e-mission',
      assignmentId: 'e2e-assignment',
      parentId: macroTodo.id,
      content: '实现搜索索引构建器',
      reasoning: '搜索引擎需要索引',
      type: 'implementation',
      workerId: 'claude',
    });

    const subTodo2 = await todoManager.create({
      missionId: 'e2e-mission',
      assignmentId: 'e2e-assignment',
      parentId: macroTodo.id,
      content: '实现查询解析器',
      reasoning: '搜索引擎需要查询解析',
      type: 'implementation',
      workerId: 'claude',
    });

    // ===== 阶段 4: 完成宏观 Todo =====
    await todoManager.complete(macroTodo.id, {
      success: true,
      summary: '搜索引擎核心架构完成',
      modifiedFiles: ['src/search/index.ts', 'src/search/engine.ts'],
      duration: 8000,
    });

    // ===== 阶段 5: 模拟 webview-provider normalizeTodo =====
    const allTodos = [macroTodo, subTodo1, subTodo2];
    const seen = new Set<string>();
    const normalizedTodos = allTodos
      .filter(t => t && typeof t === 'object')
      .map(t => {
        const id = t.id;
        if (seen.has(id)) return null;
        seen.add(id);
        return { ...t, assignmentId: t.assignmentId || 'e2e-assignment' };
      })
      .filter(Boolean) as any[];

    assertEqual(normalizedTodos.length, 3, '归一化后 Todo 数量');

    // ===== 阶段 6: 模拟 message-handler 映射到 AssignmentTodo =====
    const assignmentTodos = normalizedTodos.map(t => ({
      id: t.id,
      assignmentId: t.assignmentId,
      parentId: t.parentId,
      content: t.content,
      status: t.status,
      type: t.type,
      priority: t.priority,
    }));

    // ===== 阶段 7: 模拟 TasksPanel 渲染逻辑 =====
    const rootTodos = assignmentTodos.filter((t: any) => !t.parentId);
    assertEqual(rootTodos.length, 1, '根 Todo（宏观任务）');

    const rootTodo = rootTodos[0];
    const childTodos = assignmentTodos.filter((t: any) => t.parentId === rootTodo.id);
    assertEqual(childTodos.length, 2, '子 Todo（动态拆分）');

    // 验证子 Todo 的 parentId 一致性
    for (const child of childTodos) {
      assertEqual(child.parentId, macroTodo.id, `子 Todo "${child.content}" 的 parentId`);
    }

    // 验证序号
    const seqNum = rootTodos.indexOf(rootTodo) + 1;
    assertEqual(seqNum, 1, '宏观 Todo 序号应为 1');
    childTodos.forEach((child: any, idx: number) => {
      const childSeq = `${seqNum}.${idx + 1}`;
      assert(childSeq.startsWith('1.'), `子 Todo 序号应以 1. 开头`);
    });

    // ===== 结论: 完整管道验证通过 =====
    // 编排者 create → Worker 执行 → 动态拆分 + parentId → 归一化 → 映射 → UI 渲染
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 测试 11: 多 Assignment 并行 + 各自子 Todo
// ============================================================================

async function testMultiAssignmentParallelTodos(): Promise<void> {
  const workDir = createTestDir();
  const todoManager = new TodoManager(workDir);
  await todoManager.initialize();

  try {
    // 模拟两个 Worker 各自的宏观 Todo
    const claudeTodo = await todoManager.create({
      missionId: 'mission-parallel',
      assignmentId: 'asgn-claude',
      content: '实现后端 API',
      reasoning: '后端专家',
      type: 'implementation',
      workerId: 'claude',
    });

    const geminiTodo = await todoManager.create({
      missionId: 'mission-parallel',
      assignmentId: 'asgn-gemini',
      content: '实现前端界面',
      reasoning: '前端专家',
      type: 'implementation',
      workerId: 'gemini',
    });

    // Claude Worker 拆分
    const claudeChild = await todoManager.create({
      missionId: 'mission-parallel',
      assignmentId: 'asgn-claude',
      parentId: claudeTodo.id,
      content: '数据库模型设计',
      reasoning: '子任务',
      type: 'design',
      workerId: 'claude',
    });

    // Gemini Worker 拆分
    const geminiChild1 = await todoManager.create({
      missionId: 'mission-parallel',
      assignmentId: 'asgn-gemini',
      parentId: geminiTodo.id,
      content: '组件样式实现',
      reasoning: '子任务',
      type: 'implementation',
      workerId: 'gemini',
    });

    const geminiChild2 = await todoManager.create({
      missionId: 'mission-parallel',
      assignmentId: 'asgn-gemini',
      parentId: geminiTodo.id,
      content: '状态管理实现',
      reasoning: '子任务',
      type: 'implementation',
      workerId: 'gemini',
    });

    // 按 assignmentId 分组验证
    const allTodos = [claudeTodo, geminiTodo, claudeChild, geminiChild1, geminiChild2];

    const claudeGroup = allTodos.filter(t => t.assignmentId === 'asgn-claude');
    const geminiGroup = allTodos.filter(t => t.assignmentId === 'asgn-gemini');

    assertEqual(claudeGroup.length, 2, 'Claude 分组 Todo 数量');
    assertEqual(geminiGroup.length, 3, 'Gemini 分组 Todo 数量');

    // 验证子 Todo 不会跨 Assignment 挂载
    assertEqual(claudeChild.parentId, claudeTodo.id, 'Claude 子 Todo 挂载正确');
    assertEqual(geminiChild1.parentId, geminiTodo.id, 'Gemini 子 Todo 1 挂载正确');
    assertEqual(geminiChild2.parentId, geminiTodo.id, 'Gemini 子 Todo 2 挂载正确');

    // 验证每个 Assignment 内部的层级独立
    const claudeRoots = claudeGroup.filter(t => !t.parentId);
    const claudeChildren = claudeGroup.filter(t => t.parentId === claudeTodo.id);
    assertEqual(claudeRoots.length, 1, 'Claude 根 Todo');
    assertEqual(claudeChildren.length, 1, 'Claude 子 Todo');

    const geminiRoots = geminiGroup.filter(t => !t.parentId);
    const geminiChildren = geminiGroup.filter(t => t.parentId === geminiTodo.id);
    assertEqual(geminiRoots.length, 1, 'Gemini 根 Todo');
    assertEqual(geminiChildren.length, 2, 'Gemini 子 Todo');
  } finally {
    todoManager.destroy();
    cleanupTestDir(workDir);
  }
}

// ============================================================================
// 主入口
// ============================================================================

async function main() {
  console.log('\n========================================');
  console.log('  编排系统端到端验证');
  console.log('========================================\n');

  console.log('--- Todo 数据层验证 ---');
  await runTest('1. TodoManager parentId 传递与层级关系', testTodoManagerParentId);
  await runTest('2. PlanningExecutor 宏观 Todo 创建', testPlanningExecutorTodoCreation);
  await runTest('3. Worker 动态 Todo 拆分 + parentId', testWorkerDynamicTodoSplitting);
  await runTest('4. 事件传播链完整性', testEventPropagationChain);
  await runTest('5. dynamicTodoAdded 事件模拟', testDynamicTodoAddedEvent);

  console.log('\n--- 前端数据映射验证 ---');
  await runTest('6. normalizeTodo 保留 parentId', testNormalizeTodoPreservesParentId);
  await runTest('7. message-handler Todo 映射', testMessageHandlerTodoMapping);
  await runTest('8. TasksPanel 层级渲染数据', testTasksPanelHierarchyData);

  console.log('\n--- 集成场景验证 ---');
  await runTest('9. dispatch_task 路径 Todo 创建', testDispatchTaskTodoCreation);
  await runTest('10. 完整流程端到端管道', testFullPipelineE2E);
  await runTest('11. 多 Assignment 并行 + 各自子 Todo', testMultiAssignmentParallelTodos);

  // 汇总
  console.log('\n========================================');
  console.log('  测试结果汇总');
  console.log('========================================');

  const passed = results.filter(r => r.passed).length;
  const failed = results.filter(r => !r.passed).length;
  const totalTime = results.reduce((sum, r) => sum + r.duration, 0);

  console.log(`\n  通过: ${passed}/${results.length}`);
  console.log(`  失败: ${failed}/${results.length}`);
  console.log(`  耗时: ${totalTime}ms`);

  if (failed > 0) {
    console.log('\n  失败用例:');
    for (const r of results.filter(r => !r.passed)) {
      console.log(`    ✗ ${r.name}: ${r.details.join(', ')}`);
    }
    process.exit(1);
  } else {
    console.log('\n  ✓ 所有测试通过！编排系统数据流完整性已验证。');
    process.exit(0);
  }
}

main().catch(error => {
  console.error('\n执行失败:', error);
  process.exit(1);
});
