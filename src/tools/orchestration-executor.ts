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
export class OrchestrationExecutor {
  private dispatchHandler?: DispatchTaskHandler;
  private sendMessageHandler?: SendWorkerMessageHandler;
  private waitForWorkersHandler?: WaitForWorkersHandler;
  private splitTodoHandler?: SplitTodoHandler;
  /** 动态 Worker 列表（必须由 MissionDrivenEngine 从 ProfileLoader 注入） */
  private availableWorkers: { slot: WorkerSlot; description: string }[] = [];
  /** Category → Worker 映射（必须由 DispatchManager 从 ProfileLoader 注入） */
  private categoryWorkerMap: CategoryWorkerEntry[] = [];

  private static readonly TOOL_NAMES = ['dispatch_task', 'send_worker_message', 'wait_for_workers', 'split_todo'] as const;

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
  }): void {
    this.dispatchHandler = handlers.dispatch;
    this.sendMessageHandler = handlers.sendMessage;
    this.waitForWorkersHandler = handlers.waitForWorkers;
    this.splitTodoHandler = handlers.splitTodo;
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

    return {
      name: 'dispatch_task',
      description: '将子任务分配给专业 AI Worker 执行。通过 category 参数指定任务分类，系统自动路由到对应 Worker。Worker 将自主完成任务并在主对话区回传执行进度和结果。',
      input_schema: {
        type: 'object',
        properties: {
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
        },
        required: ['category', 'goal', 'acceptance', 'constraints', 'context', 'requires_modification'],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'worker', 'dispatch'],
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

    const args = toolCall.arguments as {
      category?: string;
      goal?: string;
      acceptance?: string[];
      constraints?: string[];
      context?: string[];
      requires_modification?: boolean;
      scope_hint?: string[];
      files?: string[];
      depends_on?: string[];
      contracts?: DispatchTaskCollaborationContracts;
    };

    // 结构化合同字段必填
    if (!args.category || typeof args.category !== 'string' || !args.category.trim()) {
      const validCategories = this.getCategoryEnum();
      return {
        toolCallId: toolCall.id,
        content: `Error: category 是必填参数。可选值: ${validCategories.join(', ')}`,
        isError: true,
      };
    }
    if (!args.goal || typeof args.goal !== 'string' || !args.goal.trim()) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: goal 是必填参数，表示任务目标',
        isError: true,
      };
    }

    const acceptanceValidation = this.normalizeStringArray(args.acceptance, 'acceptance', true);
    if (!acceptanceValidation.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${acceptanceValidation.error}`,
        isError: true,
      };
    }

    const constraintsValidation = this.normalizeStringArray(args.constraints, 'constraints', true);
    if (!constraintsValidation.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${constraintsValidation.error}`,
        isError: true,
      };
    }

    const contextValidation = this.normalizeStringArray(args.context, 'context', true);
    if (!contextValidation.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${contextValidation.error}`,
        isError: true,
      };
    }

    if (typeof args.requires_modification !== 'boolean') {
      return {
        toolCallId: toolCall.id,
        content: 'Error: requires_modification 是必填布尔参数（true/false）',
        isError: true,
      };
    }

    const scopeHintValidation = this.normalizeStringArray(args.scope_hint, 'scope_hint', false);
    if (!scopeHintValidation.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${scopeHintValidation.error}`,
        isError: true,
      };
    }

    const filesValidation = this.normalizeStringArray(args.files, 'files', false);
    if (!filesValidation.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${filesValidation.error}`,
        isError: true,
      };
    }

    const dependsOnValidation = this.normalizeStringArray(args.depends_on, 'depends_on', false);
    if (!dependsOnValidation.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${dependsOnValidation.error}`,
        isError: true,
      };
    }

    const contractsValidation = this.normalizeContracts(args.contracts);
    if (!contractsValidation.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${contractsValidation.error}`,
        isError: true,
      };
    }

    const category = args.category.trim();

    // 验证 category 在已知枚举内
    const validCategories = this.getCategoryEnum();
    if (validCategories.length > 0 && !validCategories.includes(category)) {
      return {
        toolCallId: toolCall.id,
        content: `Error: 未知分类 "${category}"。可选值: ${validCategories.join(', ')}`,
        isError: true,
      };
    }

    logger.info('dispatch_task 开始', {
      category,
      requiresModification: args.requires_modification,
      scopeHintCount: scopeHintValidation.value.length,
      goalPreview: args.goal.substring(0, 80),
      files: filesValidation.value,
      dependsOn: dependsOnValidation.value,
    }, LogCategory.TOOLS);

    try {
      const result = await this.dispatchHandler({
        worker: 'auto',
        category,
        requiresModification: args.requires_modification,
        goal: args.goal.trim(),
        acceptance: acceptanceValidation.value,
        constraints: constraintsValidation.value,
        context: contextValidation.value,
        scopeHint: scopeHintValidation.value.length > 0 ? scopeHintValidation.value : undefined,
        files: filesValidation.value.length > 0 ? filesValidation.value : undefined,
        dependsOn: dependsOnValidation.value.length > 0 ? dependsOnValidation.value : undefined,
        contracts: contractsValidation.value,
      });

      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(result),
        isError: result.status === 'failed',
      };
    } catch (error: any) {
      logger.error('dispatch_task 执行失败', { error: error.message }, LogCategory.TOOLS);
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
}
