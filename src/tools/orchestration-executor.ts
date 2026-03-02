/**
 * 编排工具执行器
 * 提供 dispatch_task、send_worker_message、wait_for_workers 三个元工具
 *
 * 这些工具使 orchestrator LLM 能够：
 * - dispatch_task: 将子任务分配给专业 Worker 执行（非阻塞）
 * - wait_for_workers: 等待已分配的 Worker 完成并获取结果（阻塞）
 * - send_worker_message: 向 Worker 面板发送消息
 *
 * 反应式编排循环：dispatch → wait → analyze results → dispatch more / finalize
 */

import { ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';
import type { WorkerSlot } from '../types';

/**
 * Category → Worker 映射条目（由 DispatchManager 从 ProfileLoader 注入）
 */
export interface CategoryWorkerEntry {
  category: string;
  displayName: string;
  worker: WorkerSlot;
}

/**
 * dispatch_task 任务合同（结构化）
 */
export interface DispatchTaskContractInput {
  /** 简短任务名称 */
  task_name: string;
  /** 任务目标（业务结果） */
  goal: string;
  /** 验收标准 */
  acceptance: string[];
  /** 约束条件 */
  constraints: string[];
  /** 已知上下文 */
  context: string[];
}

/**
 * 多 Worker 协作契约（可选）
 */
export interface DispatchTaskCollaborationContracts {
  /** 当前任务作为生产者输出的契约 */
  producer_contracts?: string[];
  /** 当前任务作为消费者依赖的契约 */
  consumer_contracts?: string[];
  /** 协作接口约定（文字描述） */
  interface_contracts?: string[];
  /** 冻结区域（本任务不可修改的文件） */
  freeze_files?: string[];
}

/**
 * dispatch_task 回调：由 MissionDrivenEngine 注入，实际执行 Worker 委派
 * 返回 task_id 后立即结束（非阻塞），Worker 在后台异步执行
 */
export type DispatchTaskHandler = (params: {
  worker: 'auto';
  category: string;
  /** 是否要求子任务对目标文件产生实际修改 */
  requiresModification: boolean;
  /** 结构化合同字段 */
  task_name: string;
  goal: string;
  acceptance: string[];
  constraints: string[];
  context: string[];
  /**
   * 范围线索（非硬约束）
   * 用于提示 Worker 优先关注的文件/目录，可在执行中自然扩展
   */
  scopeHint?: string[];
  /**
   * 严格目标文件（可选）
   * 当任务确实要求“必须改这些文件”时使用；否则优先使用 scopeHint
   */
  files?: string[];
  dependsOn?: string[];
  /** 跨任务协作契约（L3） */
  contracts?: DispatchTaskCollaborationContracts;
}) => Promise<{
  task_id: string;
  status: 'dispatched' | 'failed';
  /** 实际执行的 Worker（可能与请求值不同） */
  worker?: WorkerSlot;
  /** 路由分类（用于审计与可解释性） */
  category?: string;
  /** 任务简短名称 */
  task_name?: string;
  /** 路由解释（含降级原因） */
  routing_reason?: string;
  /** 是否触发了降级改派 */
  degraded?: boolean;
  error?: string;
}>;

/**
 * send_worker_message 回调：由 MissionDrivenEngine 注入，向 Worker 面板发送消息
 */
export type SendWorkerMessageHandler = (params: {
  worker: WorkerSlot;
  message: string;
}) => Promise<{
  delivered: boolean;
}>;

/**
 * wait_for_workers 单个 Worker 完成结果
 */
export interface WorkerCompletionResult {
  task_id: string;
  worker: WorkerSlot;
  status: 'completed' | 'failed' | 'skipped' | 'cancelled';
  summary: string;
  modified_files: string[];
  errors?: string[];
}

/**
 * wait_for_workers 返回结构
 */
export interface WaitForWorkersResult {
  results: WorkerCompletionResult[];
  /** completed: 已满足等待条件；timeout: 到达超时阈值仍未满足 */
  wait_status: 'completed' | 'timeout';
  timed_out: boolean;
  /** 本次等待目标中仍未完成的任务 ID */
  pending_task_ids: string[];
  /** 阻塞耗时（毫秒） */
  waited_ms: number;
  /** 全量完成时的程序化审计结论（可选） */
  audit?: {
    level: 'normal' | 'watch' | 'intervention';
    summary: {
      normal: number;
      watch: number;
      intervention: number;
    };
    issues: Array<{
      task_id: string;
      level: 'normal' | 'watch' | 'intervention';
      dimension: 'scope' | 'cross_task' | 'contract';
      detail: string;
    }>;
  };
}

/**
 * wait_for_workers 回调：阻塞直到指定（或全部）Worker 完成
 */
export type WaitForWorkersHandler = (params: {
  task_ids?: string[];
}) => Promise<WaitForWorkersResult>;

/**
 * split_todo 调用方上下文（标识调用者所属的 mission/assignment/todo/worker）
 */
export interface SplitTodoCallerContext {
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;
}

/**
 * split_todo 回调：Worker 将当前 Todo 拆分为多个子步骤
 */
export type SplitTodoHandler = (params: {
  subtasks: Array<{
    content: string;
    reasoning: string;
    type: 'implementation' | 'verification' | 'discovery';
  }>;
  callerContext: SplitTodoCallerContext;
}) => Promise<{
  success: boolean;
  childTodoIds: string[];
  error?: string;
}>;

/**
 * 编排工具执行器
 */
export type GetTodosHandler = (params: {
  missionId?: string;
  sessionId?: string;
  status?: string[];
  callerContext?: Pick<SplitTodoCallerContext, 'missionId' | 'assignmentId' | 'workerId'>;
}) => Promise<any[]>;

export type UpdateTodoStatus = 'pending' | 'skipped';

export type UpdateTodoHandler = (params: {
  updates: Array<{
    todoId: string;
    status?: UpdateTodoStatus;
    content?: string;
  }>;
}) => Promise<{ success: boolean; error?: string }>;

export class OrchestrationExecutor {
  private dispatchHandler?: DispatchTaskHandler;
  private sendMessageHandler?: SendWorkerMessageHandler;
  private waitForWorkersHandler?: WaitForWorkersHandler;
  private splitTodoHandler?: SplitTodoHandler;
  private getTodosHandler?: GetTodosHandler;
  private updateTodoHandler?: UpdateTodoHandler;

  /** 动态 Worker 列表（必须由 MissionDrivenEngine 从 ProfileLoader 注入） */
  private availableWorkers: { slot: WorkerSlot; description: string }[] = [];
  /** Category → Worker 映射（必须由 DispatchManager 从 ProfileLoader 注入） */
  private categoryWorkerMap: CategoryWorkerEntry[] = [];

  private static readonly TOOL_NAMES = ['dispatch_task', 'send_worker_message', 'wait_for_workers', 'split_todo', 'get_todos', 'update_todo'] as const;
  private static readonly UPDATE_TODO_STATUS_ENUM: UpdateTodoStatus[] = ['pending', 'skipped'];

  /**
   * 设置可用 Worker 列表（由 MissionDrivenEngine 从 ProfileLoader 注入）
   */
  setAvailableWorkers(workers: { slot: WorkerSlot; description: string }[]): void {
    // 必须无条件覆盖，避免”全禁用后仍保留旧枚举”的陈旧状态
    this.availableWorkers = workers;
  }

  /**
   * 设置 Category → Worker 映射（由 DispatchManager 从 ProfileLoader 注入）
   * 用于 dispatch_task 工具 schema 的 category enum 和描述
   */
  setCategoryWorkerMap(map: CategoryWorkerEntry[]): void {
    this.categoryWorkerMap = map;
  }

  private getWorkerEnum(): string[] {
    if (this.availableWorkers.length === 0) {
      logger.warn('OrchestrationExecutor.getWorkerEnum: Worker 列表未注入，使用空列表', undefined, LogCategory.TOOLS);
    }
    return this.availableWorkers.map(w => w.slot);
  }

  private getCategoryEnum(): string[] {
    return this.categoryWorkerMap.map(e => e.category);
  }

  /**
   * 构建 category 参数的分工映射描述
   * 按 Worker 分组，格式：worker: category1(显示名)/category2(显示名)
   */
  private getCategoryMappingDescription(): string {
    const byWorker = new Map<string, CategoryWorkerEntry[]>();
    for (const entry of this.categoryWorkerMap) {
      const list = byWorker.get(entry.worker) || [];
      list.push(entry);
      byWorker.set(entry.worker, list);
    }
    return Array.from(byWorker.entries())
      .map(([worker, entries]) => {
        const categories = entries.map(e => `${e.category}(${e.displayName})`).join('/');
        return `${categories} → ${worker}`;
      })
      .join('；');
  }

  /**
   * 注入回调处理器
   */
  setHandlers(handlers: {
    dispatch?: DispatchTaskHandler;
    sendMessage?: SendWorkerMessageHandler;
    waitForWorkers?: WaitForWorkersHandler;
    splitTodo?: SplitTodoHandler;
    getTodos?: GetTodosHandler;
    updateTodo?: UpdateTodoHandler;
  }): void {
    this.dispatchHandler = handlers.dispatch;
    this.sendMessageHandler = handlers.sendMessage;
    this.waitForWorkersHandler = handlers.waitForWorkers;
    this.splitTodoHandler = handlers.splitTodo;
    this.getTodosHandler = handlers.getTodos;
    this.updateTodoHandler = handlers.updateTodo;
  }

  /**
   * 检查工具名是否属于编排工具
   */
  isOrchestrationTool(toolName: string): boolean {
    return (OrchestrationExecutor.TOOL_NAMES as readonly string[]).includes(toolName);
  }

  /**
   * 获取所有编排工具定义
   */
  getToolDefinitions(): ExtendedToolDefinition[] {
    return [
      this.getDispatchTaskDefinition(),
      this.getWaitForWorkersDefinition(),
      this.getSendWorkerMessageDefinition(),
      this.getSplitTodoDefinition(),
      this.getGetTodosDefinition(),
      this.getUpdateTodoDefinition(),
    ];
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall, callerContext?: SplitTodoCallerContext): Promise<ToolResult> {
    logger.debug('OrchestrationExecutor 执行', {
      toolName: toolCall.name,
      toolCallId: toolCall.id,
    }, LogCategory.TOOLS);

    switch (toolCall.name) {
      case 'dispatch_task':
        return this.executeDispatchTask(toolCall);
      case 'wait_for_workers':
        return this.executeWaitForWorkers(toolCall);
      case 'send_worker_message':
        return this.executeSendWorkerMessage(toolCall);
      case 'split_todo':
        return this.executeSplitTodo(toolCall, callerContext);
      case 'get_todos':
        return this.executeGetTodos(toolCall, callerContext);
      case 'update_todo':
        return this.executeUpdateTodo(toolCall);
      default:
        return {
          toolCallId: toolCall.id,
          content: `Unknown orchestration tool: ${toolCall.name}`,
          isError: true,
        };
    }
  }

  // ===========================================================================
  // dispatch_task
  // ===========================================================================

  private getDispatchTaskDefinition(): ExtendedToolDefinition {
    const categoryEnum = this.getCategoryEnum();
    const mappingDesc = this.getCategoryMappingDescription();

    const taskItemProperties: Record<string, any> = {
      task_name: {
        type: 'string',
        description: '标准的工程化任务名称，简短概括任务内容（例如：重构用户登录模块，修复导航栏溢出 Bug 等），不要照抄用户原始对话。'
      },
      category: {
        type: 'string',
        ...(categoryEnum.length > 0 ? { enum: categoryEnum } : {}),
        description: `任务分类（决定执行 Worker 的唯一依据）。分工映射：${mappingDesc || '未配置'}`,
      },
      goal: {
        type: 'string',
        description: '任务目标（Goal）：要达成的业务结果',
      },
      acceptance: {
        type: 'array',
        items: { type: 'string' },
        description: '验收标准（Acceptance）：明确完成判定条件，至少 1 条',
      },
      constraints: {
        type: 'array',
        items: { type: 'string' },
        description: '约束条件（Constraints）：必须遵守的规则，至少 1 条',
      },
      context: {
        type: 'array',
        items: { type: 'string' },
        description: '任务上下文（Context）：已知事实、线索、关联信息，至少 1 条',
      },
      requires_modification: {
        type: 'boolean',
        description: '是否要求该任务对目标文件产生实际修改。只读分析/统计/总结任务必须传 false；功能开发/修复/重构任务传 true。',
      },
      scope_hint: {
        type: 'array',
        items: { type: 'string' },
        description: '范围线索（非硬约束）。建议提供优先关注的文件/目录，Worker 可在执行中自然扩展。',
      },
      files: {
        type: 'array',
        items: { type: 'string' },
        description: '严格目标文件（可选）。仅在确需限定目标文件时提供；否则建议使用 scope_hint。',
      },
      depends_on: {
        type: 'array',
        items: { type: 'string' },
        description: '依赖的前序任务 task_id 列表。被依赖的任务完成后本任务才会执行，可通过 SharedContextPool 获取前序任务的输出上下文',
      },
      contracts: {
        type: 'object',
        description: '协作契约（L3 任务可选）：接口约定、冻结区域、生产/消费契约标识',
        properties: {
          producer_contracts: {
            type: 'array',
            items: { type: 'string' },
            description: '当前任务产出的契约标识',
          },
          consumer_contracts: {
            type: 'array',
            items: { type: 'string' },
            description: '当前任务依赖的契约标识',
          },
          interface_contracts: {
            type: 'array',
            items: { type: 'string' },
            description: '接口约定文本（签名、字段、路径等）',
          },
          freeze_files: {
            type: 'array',
            items: { type: 'string' },
            description: '冻结文件列表：本任务不得修改这些文件',
          },
        },
      },
    };

    return {
      name: 'dispatch_task',
      description: '向一个或多个专业 AI Worker 派发任务。支持一次性派发多个任务以实现并行处理。通过 category 参数指定任务分类，系统自动路由到对应 Worker。',
      input_schema: {
        type: 'object',
        properties: {
          tasks: {
            type: 'array',
            description: '待派发的任务列表（至少 1 个）。每个任务将独立路由到对应 Worker 并行执行。',
            items: {
              type: 'object',
              properties: taskItemProperties,
              required: ['task_name', 'category', 'goal', 'acceptance', 'constraints', 'context', 'requires_modification'],
            },
          },
        },
        required: ['tasks'],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'worker', 'dispatch'],
      },
    };
  }

  /**
   * 验证单个任务参数，返回规范化后的 handler 入参或错误描述
   */
  private validateSingleTaskArgs(
    task: Record<string, any>,
    index: number,
  ): { ok: true; params: Parameters<DispatchTaskHandler>[0] } | { ok: false; error: string } {
    const prefix = `tasks[${index}]`;

    if (!task.task_name || typeof task.task_name !== 'string' || !task.task_name.trim()) {
      return { ok: false, error: `${prefix}: task_name 是必填参数` };
    }
    if (!task.category || typeof task.category !== 'string' || !task.category.trim()) {
      const validCategories = this.getCategoryEnum();
      return { ok: false, error: `${prefix}: category 是必填参数。可选值: ${validCategories.join(', ')}` };
    }
    if (!task.goal || typeof task.goal !== 'string' || !task.goal.trim()) {
      return { ok: false, error: `${prefix}: goal 是必填参数` };
    }

    const acceptanceValidation = this.normalizeStringArray(task.acceptance, `${prefix}.acceptance`, true);
    if (!acceptanceValidation.ok) return acceptanceValidation;

    const constraintsValidation = this.normalizeStringArray(task.constraints, `${prefix}.constraints`, true);
    if (!constraintsValidation.ok) return constraintsValidation;

    const contextValidation = this.normalizeStringArray(task.context, `${prefix}.context`, true);
    if (!contextValidation.ok) return contextValidation;

    if (typeof task.requires_modification !== 'boolean') {
      return { ok: false, error: `${prefix}: requires_modification 是必填布尔参数（true/false）` };
    }

    const scopeHintValidation = this.normalizeStringArray(task.scope_hint, `${prefix}.scope_hint`, false);
    if (!scopeHintValidation.ok) return scopeHintValidation;

    const filesValidation = this.normalizeStringArray(task.files, `${prefix}.files`, false);
    if (!filesValidation.ok) return filesValidation;

    const dependsOnValidation = this.normalizeStringArray(task.depends_on, `${prefix}.depends_on`, false);
    if (!dependsOnValidation.ok) return dependsOnValidation;

    const contractsValidation = this.normalizeContracts(task.contracts);
    if (!contractsValidation.ok) return contractsValidation;

    const category = task.category.trim();
    const validCategories = this.getCategoryEnum();
    if (validCategories.length > 0 && !validCategories.includes(category)) {
      return { ok: false, error: `${prefix}: 未知分类 "${category}"。可选值: ${validCategories.join(', ')}` };
    }

    return {
      ok: true,
      params: {
        worker: 'auto',
        category,
        requiresModification: task.requires_modification,
        task_name: task.task_name.trim(),
        goal: task.goal.trim(),
        acceptance: acceptanceValidation.value,
        constraints: constraintsValidation.value,
        context: contextValidation.value,
        scopeHint: scopeHintValidation.value.length > 0 ? scopeHintValidation.value : undefined,
        files: filesValidation.value.length > 0 ? filesValidation.value : undefined,
        dependsOn: dependsOnValidation.value.length > 0 ? dependsOnValidation.value : undefined,
        contracts: contractsValidation.value,
      },
    };
  }

  private async executeDispatchTask(toolCall: ToolCall): Promise<ToolResult> {
    if (!this.dispatchHandler) {
      return {
        toolCallId: toolCall.id,
        content: 'dispatch_task handler not configured',
        isError: true,
      };
    }

    const args = toolCall.arguments as { tasks?: Record<string, any>[] };

    if (!Array.isArray(args.tasks) || args.tasks.length === 0) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: tasks 必须是至少包含 1 个元素的数组',
        isError: true,
      };
    }

    // 阶段 1：全量验证（任一任务校验失败则整批拒绝，避免部分派发）
    const validatedParams: Parameters<DispatchTaskHandler>[0][] = [];
    for (let i = 0; i < args.tasks.length; i++) {
      const validation = this.validateSingleTaskArgs(args.tasks[i], i);
      if (!validation.ok) {
        return {
          toolCallId: toolCall.id,
          content: `Error: ${validation.error}`,
          isError: true,
        };
      }
      validatedParams.push(validation.params);
    }

    logger.info('dispatch_task 开始批量派发', {
      taskCount: validatedParams.length,
      categories: validatedParams.map(p => p.category),
      taskNames: validatedParams.map(p => p.task_name),
    }, LogCategory.TOOLS);

    // 阶段 2：并行派发所有任务
    try {
      const results = await Promise.all(
        validatedParams.map(params => this.dispatchHandler!(params))
      );

      const hasFailure = results.some(r => r.status === 'failed');
      return {
        toolCallId: toolCall.id,
        content: JSON.stringify({ results }),
        isError: hasFailure,
      };
    } catch (error: any) {
      logger.error('dispatch_task 批量执行失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `dispatch_task failed: ${error.message}`,
        isError: true,
      };
    }
  }

  private normalizeStringArray(
    raw: unknown,
    fieldName: string,
    required: boolean,
  ): { ok: true; value: string[] } | { ok: false; error: string } {
    if (raw === undefined || raw === null) {
      if (required) {
        return { ok: false, error: `${fieldName} 是必填字符串数组` };
      }
      return { ok: true, value: [] };
    }

    if (!Array.isArray(raw)) {
      return { ok: false, error: `${fieldName} 必须是字符串数组` };
    }

    const normalized = raw
      .map(item => typeof item === 'string' ? item.trim() : '')
      .filter(Boolean);

    if (normalized.length !== raw.length) {
      return { ok: false, error: `${fieldName} 数组中存在空值或非字符串项` };
    }

    if (required && normalized.length === 0) {
      return { ok: false, error: `${fieldName} 不能为空数组` };
    }

    return { ok: true, value: normalized };
  }

  private normalizeContracts(
    raw: unknown,
  ): { ok: true; value?: DispatchTaskCollaborationContracts } | { ok: false; error: string } {
    if (raw === undefined || raw === null) {
      return { ok: true, value: undefined };
    }

    if (typeof raw !== 'object' || Array.isArray(raw)) {
      return { ok: false, error: 'contracts 必须是对象' };
    }

    const obj = raw as DispatchTaskCollaborationContracts;
    const producer = this.normalizeStringArray(obj.producer_contracts, 'contracts.producer_contracts', false);
    if (!producer.ok) return producer;
    const consumer = this.normalizeStringArray(obj.consumer_contracts, 'contracts.consumer_contracts', false);
    if (!consumer.ok) return consumer;
    const iface = this.normalizeStringArray(obj.interface_contracts, 'contracts.interface_contracts', false);
    if (!iface.ok) return iface;
    const freeze = this.normalizeStringArray(obj.freeze_files, 'contracts.freeze_files', false);
    if (!freeze.ok) return freeze;

    if (
      producer.value.length === 0
      && consumer.value.length === 0
      && iface.value.length === 0
      && freeze.value.length === 0
    ) {
      return { ok: true, value: undefined };
    }

    return {
      ok: true,
      value: {
        ...(producer.value.length > 0 ? { producer_contracts: producer.value } : {}),
        ...(consumer.value.length > 0 ? { consumer_contracts: consumer.value } : {}),
        ...(iface.value.length > 0 ? { interface_contracts: iface.value } : {}),
        ...(freeze.value.length > 0 ? { freeze_files: freeze.value } : {}),
      },
    };
  }

  // ===========================================================================
  // wait_for_workers
  // ===========================================================================

  private getWaitForWorkersDefinition(): ExtendedToolDefinition {
    return {
      name: 'wait_for_workers',
      description: '等待已分配的 Worker 完成执行并返回结果。这是反应式编排的核心工具：dispatch_task 发送任务后，调用此工具阻塞等待结果，然后根据结果决定是否追加新任务或结束。不传 task_ids 则等待当前批次全部完成。返回包含 wait_status（completed/timeout）和 pending_task_ids，timeout 时必须继续决策，不可当作全部完成。全量完成时额外返回 audit（程序化审计结论）。',
      input_schema: {
        type: 'object',
        properties: {
          task_ids: {
            type: 'array',
            items: { type: 'string' },
            description: '等待的 task_id 列表（由 dispatch_task 返回）。不传则等待当前批次中所有任务完成',
          },
        },
        required: [],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'worker', 'coordination', 'reactive'],
      },
    };
  }

  private async executeWaitForWorkers(toolCall: ToolCall): Promise<ToolResult> {
    if (!this.waitForWorkersHandler) {
      return {
        toolCallId: toolCall.id,
        content: 'wait_for_workers handler not configured',
        isError: true,
      };
    }

    const args = toolCall.arguments as { task_ids?: string[] };

    logger.info('wait_for_workers 开始等待', {
      taskIds: args.task_ids || 'all',
    }, LogCategory.TOOLS);

    try {
      const result = await this.waitForWorkersHandler({
        task_ids: args.task_ids,
      });

      logger.info('wait_for_workers 完成', {
        waitStatus: result.wait_status,
        timedOut: result.timed_out,
        pendingTaskIds: result.pending_task_ids,
        waitedMs: result.waited_ms,
        resultCount: result.results.length,
        successes: result.results.filter(r => r.status === 'completed').length,
        failures: result.results.filter(r => r.status === 'failed').length,
      }, LogCategory.TOOLS);

      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(result),
        isError: false,
      };
    } catch (error: any) {
      logger.error('wait_for_workers 失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `wait_for_workers failed: ${error.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // send_worker_message
  // ===========================================================================

  private getSendWorkerMessageDefinition(): ExtendedToolDefinition {
    return {
      name: 'send_worker_message',
      description: '向指定 Worker 的面板发送消息。用于传递补充上下文、调整指令或协作信息。',
      input_schema: {
        type: 'object',
        properties: {
          worker: {
            type: 'string',
            enum: this.getWorkerEnum(),
            description: '目标 Worker',
          },
          message: {
            type: 'string',
            description: '要发送的消息内容',
          },
        },
        required: ['worker', 'message'],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'worker', 'communication'],
      },
    };
  }

  private async executeSendWorkerMessage(toolCall: ToolCall): Promise<ToolResult> {
    if (!this.sendMessageHandler) {
      return {
        toolCallId: toolCall.id,
        content: 'send_worker_message handler not configured',
        isError: true,
      };
    }

    const args = toolCall.arguments as { worker: string; message: string };

    if (!args.worker || !args.message) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: worker and message are required',
        isError: true,
      };
    }

    const validWorkers = this.getWorkerEnum();
    if (!validWorkers.includes(args.worker)) {
      return {
        toolCallId: toolCall.id,
        content: `Error: invalid worker "${args.worker}". Must be one of: ${validWorkers.join(', ')}`,
        isError: true,
      };
    }

    logger.info('send_worker_message', {
      worker: args.worker,
      messagePreview: args.message.substring(0, 80),
    }, LogCategory.TOOLS);

    try {
      const result = await this.sendMessageHandler({
        worker: args.worker as WorkerSlot,
        message: args.message,
      });

      return {
        toolCallId: toolCall.id,
        content: JSON.stringify({ delivered: result.delivered }),
        isError: false,
      };
    } catch (error: any) {
      logger.error('send_worker_message 失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `send_worker_message failed: ${error.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // split_todo
  // ===========================================================================

  private getSplitTodoDefinition(): ExtendedToolDefinition {
    return {
      name: 'split_todo',
      description: '将当前任务拆分为多个子步骤。当任务包含多个可独立完成和验证的子目标时使用。拆分后每个子步骤将依次执行，全部完成后父任务自动标记完成。',
      input_schema: {
        type: 'object',
        properties: {
          subtasks: {
            type: 'array',
            items: {
              type: 'object',
              properties: {
                content: {
                  type: 'string',
                  description: '子步骤的具体内容',
                },
                reasoning: {
                  type: 'string',
                  description: '拆分出此子步骤的原因',
                },
                type: {
                  type: 'string',
                  enum: ['implementation', 'verification', 'discovery'],
                  description: '子步骤类型：implementation(实现)、verification(验证)、discovery(探索/分析)',
                },
              },
              required: ['content', 'reasoning', 'type'],
            },
            description: '子步骤列表（至少 2 个）',
          },
        },
        required: ['subtasks'],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'worker', 'todo', 'split'],
      },
    };
  }

  private async executeSplitTodo(toolCall: ToolCall, callerContext?: SplitTodoCallerContext): Promise<ToolResult> {
    if (!this.splitTodoHandler) {
      return {
        toolCallId: toolCall.id,
        content: 'split_todo handler not configured',
        isError: true,
      };
    }

    if (!callerContext) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: split_todo 需要执行上下文（仅 Worker 可调用）',
        isError: true,
      };
    }

    const args = toolCall.arguments as {
      subtasks?: Array<{
        content?: string;
        reasoning?: string;
        type?: string;
      }>;
    };

    if (!Array.isArray(args.subtasks) || args.subtasks.length < 2) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: subtasks 必须是至少包含 2 个元素的数组',
        isError: true,
      };
    }

    const validTypes = ['implementation', 'verification', 'discovery'];
    for (const subtask of args.subtasks) {
      if (!subtask.content || typeof subtask.content !== 'string' || !subtask.content.trim()) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: 每个子步骤必须有非空的 content',
          isError: true,
        };
      }
      if (!subtask.reasoning || typeof subtask.reasoning !== 'string') {
        return {
          toolCallId: toolCall.id,
          content: 'Error: 每个子步骤必须有 reasoning',
          isError: true,
        };
      }
      if (!subtask.type || !validTypes.includes(subtask.type)) {
        return {
          toolCallId: toolCall.id,
          content: `Error: 每个子步骤的 type 必须是 ${validTypes.join('/')}`,
          isError: true,
        };
      }
    }

    logger.info('split_todo 开始', {
      todoId: callerContext.todoId,
      subtaskCount: args.subtasks.length,
      workerId: callerContext.workerId,
    }, LogCategory.TOOLS);

    try {
      const result = await this.splitTodoHandler({
        subtasks: args.subtasks.map(s => ({
          content: s.content!.trim(),
          reasoning: s.reasoning!,
          type: s.type as 'implementation' | 'verification' | 'discovery',
        })),
        callerContext,
      });

      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(result),
        isError: !result.success,
      };
    } catch (error: any) {
      logger.error('split_todo 执行失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `split_todo failed: ${error.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // get_todos
  // ===========================================================================

  private getGetTodosDefinition(): ExtendedToolDefinition {
    return {
      name: 'get_todos',
      description: '获取 Todo 列表。支持按 mission 查询；未指定 mission_id 时，编排者默认查询当前会话全部任务的 Todos。',
      input_schema: {
        type: 'object',
        properties: {
          mission_id: {
            type: 'string',
            description: '可选：按 mission_id 精确查询'
          },
          session_id: {
            type: 'string',
            description: '可选：按 session_id 聚合查询该会话下所有 mission 的 Todos（编排者场景）'
          },
          status: {
            type: 'array',
            items: { type: 'string', enum: ['pending', 'blocked', 'ready', 'running', 'completed', 'failed', 'skipped'] },
            description: '可选：按状态过滤'
          }
        }
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'todo', 'query'],
      },
    };
  }

  private async executeGetTodos(
    toolCall: ToolCall,
    callerContext?: SplitTodoCallerContext
  ): Promise<ToolResult> {
    if (!this.getTodosHandler) {
      return {
        toolCallId: toolCall.id,
        content: 'get_todos handler not configured',
        isError: true,
      };
    }
    const args = toolCall.arguments as { status?: string[]; mission_id?: string; session_id?: string };
    try {
      const todos = await this.getTodosHandler({
        status: args.status,
        missionId: args.mission_id,
        sessionId: args.session_id,
        callerContext: callerContext
          ? {
            missionId: callerContext.missionId,
            assignmentId: callerContext.assignmentId,
            workerId: callerContext.workerId,
          }
          : undefined,
      });
      // 提炼核心字段，避免返回过多无关信息导致 token 爆炸
      const summary = todos.map(t => ({
        id: t.id,
        missionId: t.missionId,
        assignmentId: t.assignmentId,
        content: t.content,
        status: t.status,
        worker: t.workerId
      }));
      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(summary),
        isError: false,
      };
    } catch (err: any) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${err.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // update_todo
  // ===========================================================================

  private getUpdateTodoDefinition(): ExtendedToolDefinition {
    return {
      name: 'update_todo',
      description: '批量更新现有 Todo 的状态或内容（如手动标记为跳过）。',
      input_schema: {
        type: 'object',
        properties: {
          updates: {
            type: 'array',
            description: '包含要更新的 Todo 列表',
            items: {
              type: 'object',
              properties: {
                todo_id: { type: 'string', description: '待更新的 Todo ID' },
                status: {
                  type: 'string',
                  enum: OrchestrationExecutor.UPDATE_TODO_STATUS_ENUM,
                  description: '更改状态：pending 表示重置为待执行，skipped 表示跳过。completed 仅允许执行链自动推进。',
                },
                content: { type: 'string', description: '更新任务描述' }
              },
              required: ['todo_id']
            }
          }
        },
        required: ['updates']
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'todo', 'update'],
      },
    };
  }

  private async executeUpdateTodo(toolCall: ToolCall): Promise<ToolResult> {
    if (!this.updateTodoHandler) {
      return {
        toolCallId: toolCall.id,
        content: 'update_todo handler not configured',
        isError: true,
      };
    }
    const args = toolCall.arguments as { updates: Array<{ todo_id: string; status?: UpdateTodoStatus | string; content?: string }> };
    try {
      if (!args.updates || !Array.isArray(args.updates)) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: updates 必须是数组',
          isError: true,
        };
      }

      const allowedStatus = new Set(OrchestrationExecutor.UPDATE_TODO_STATUS_ENUM);
      for (const update of args.updates) {
        if (update.status !== undefined && !allowedStatus.has(update.status as UpdateTodoStatus)) {
          return {
            toolCallId: toolCall.id,
            content: `Error: status 仅支持 ${OrchestrationExecutor.UPDATE_TODO_STATUS_ENUM.join(', ')}，不允许 ${update.status}`,
            isError: true,
          };
        }
      }

      const formattedUpdates = args.updates.map(u => ({
        todoId: u.todo_id,
        status: u.status as UpdateTodoStatus | undefined,
        content: u.content
      }));
      const result = await this.updateTodoHandler({
        updates: formattedUpdates
      });
      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(result),
        isError: !result.success,
      };
    } catch (err: any) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${err.message}`,
        isError: true,
      };
    }
  }
}
