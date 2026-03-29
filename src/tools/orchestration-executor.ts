/**
 * 编排工具执行器
 * 提供 worker_dispatch、worker_send_message、worker_wait 等编排元工具
 *
 * 这些工具使 orchestrator LLM 能够：
 * - worker_dispatch: 将子任务分配给专业 Worker 执行（非阻塞）
 * - worker_wait: 等待已分配的 Worker 完成并获取结果（阻塞）
 * - worker_send_message: 向 Worker 面板发送消息
 * - context_compact: 手动触发上下文压缩与归档
 *
 * 反应式编排循环：dispatch → wait → analyze results → dispatch more / finalize
 */

import { ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';
import { repairJSON } from '../llm/protocol/adapters/protocol-utils';
import type { WorkerSlot } from '../types';

/**
 * worker_dispatch 任务合同（结构化）
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
 * worker_dispatch 回调：由 MissionDrivenEngine 注入，实际执行 Worker 委派
 * 返回 task_id 后立即结束（非阻塞），Worker 在后台异步执行
 */
export type DispatchTaskHandler = (params: {
  worker: 'auto';
  /** ownership hint（LLM 提供的归属建议，可为 'auto'） */
  ownershipHint: string;
  /** mode hint（LLM 提供的执行模式建议，可为 'auto'） */
  modeHint: string;
  /** 幂等键：用于跨重试/重放去重，建议由调用方稳定提供 */
  idempotencyKey?: string;
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
   * 当任务确实要求”必须改这些文件”时使用；否则优先使用 scopeHint
   */
  files?: string[];
  dependsOn?: string[];
  /** 跨任务协作契约（L3） */
  contracts?: DispatchTaskCollaborationContracts;
  /** 本轮任务的整体标题（仅首次 dispatch 时由编排者提供，用于替换计划账本中的临时 summary） */
  missionTitle?: string;
}) => Promise<{
  task_id: string;
  status: 'dispatched' | 'failed';
  /** 实际执行的 Worker（可能与请求值不同） */
  worker?: WorkerSlot;
  /** 路由归属 */
  ownership?: string;
  /** 执行模式 */
  mode?: string;
  /** 任务简短名称 */
  task_name?: string;
  /** 路由解释（含降级原因） */
  routing_reason?: string;
  /** 是否触发了降级改派 */
  degraded?: boolean;
  error?: string;
}>;

/**
 * worker_send_message 回调：由 MissionDrivenEngine 注入，向 Worker 运行时与面板发送消息
 */
export type SendWorkerMessageHandler = (params: {
  worker: WorkerSlot;
  message: string;
}) => Promise<{
  delivered: boolean;
}>;

/**
 * worker_wait 单个 Worker 完成结果
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
 * worker_wait 返回结构
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
      dimension: 'scope' | 'cross_task' | 'contract' | 'verification';
      detail: string;
    }>;
  };
}

/**
 * worker_wait 回调：阻塞直到指定（或全部）Worker 完成
 */
export type WaitForWorkersHandler = (params: {
  task_ids?: string[];
}) => Promise<WaitForWorkersResult>;

/**
 * todo_split 调用方上下文（标识调用者所属的 mission/assignment/todo/worker）
 */
export interface SplitTodoCallerContext {
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;
}

/**
 * todo_split 回调：Worker 将当前 Todo 拆分为多个子步骤
 */
export type SplitTodoHandler = (params: {
  subtasks: Array<{
    content: string;
    reasoning: string;
    expectedOutput: string;
    targetFiles?: string[];
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
    forceReset?: boolean;
  }>;
}) => Promise<{ success: boolean; error?: string }>;

/**
 * todo_claim_next 回调：Worker 主动认领下一个可执行的 Todo
 */
export type ClaimNextTodoHandler = (params: {
  missionId: string;
  workerId: string;
  callerContext: SplitTodoCallerContext;
}) => Promise<{
  claimed: boolean;
  todo?: {
    id: string;
    content: string;
    type: string;
    source: string;
    priority: number;
    expectedOutput?: string;
    targetFiles?: string[];
    dependsOn: string[];
  };
  affinity?: {
    level: string;
    reason: string;
  };
  remaining: number;
  reason?: string;
  error?: string;
}>;

export type CompactContextHandler = (params: {
  force?: boolean;
  reason?: string;
}) => Promise<{
  success: boolean;
  compressed: boolean;
  reason?: string;
  force?: boolean;
  beforeTokens?: number;
  afterTokens?: number;
  method?: string;
  archived?: boolean;
  archivePath?: string;
  error?: string;
}>;

export class OrchestrationExecutor {
  private dispatchHandler: DispatchTaskHandler;
  private sendMessageHandler: SendWorkerMessageHandler;
  private waitForWorkersHandler: WaitForWorkersHandler;
  private splitTodoHandler: SplitTodoHandler;
  private getTodosHandler: GetTodosHandler;
  private updateTodoHandler: UpdateTodoHandler;
  private claimNextTodoHandler: ClaimNextTodoHandler;
  private compactContextHandler: CompactContextHandler;

  /** 动态 Worker 列表（必须由 MissionDrivenEngine 从 ProfileLoader 注入） */
  private availableWorkers: { slot: WorkerSlot; description: string }[] = [];
  /** 最近一次 todo_list 快照（用于 todo_update 前置校验） */
  private lastTodoSnapshot: {
    ids: Set<string>;
    total: number;
    updatedAt: number;
    scope?: { missionId?: string; sessionId?: string };
  } | null = null;

  private static readonly TOOL_NAMES = ['worker_dispatch', 'worker_send_message', 'worker_wait', 'todo_split', 'todo_list', 'todo_update', 'context_compact', 'todo_claim_next'] as const;
  private static readonly UPDATE_TODO_STATUS_ENUM: UpdateTodoStatus[] = ['pending', 'skipped'];

  constructor() {
    const runtimeUnavailable = 'orchestration runtime unavailable';

    this.dispatchHandler = async () => ({
      task_id: '',
      status: 'failed',
      error: runtimeUnavailable,
    });

    this.sendMessageHandler = async () => ({
      delivered: false,
    });

    this.waitForWorkersHandler = async () => ({
      results: [],
      wait_status: 'completed',
      timed_out: false,
      pending_task_ids: [],
      waited_ms: 0,
    });

    this.splitTodoHandler = async () => ({
      success: false,
      childTodoIds: [],
      error: runtimeUnavailable,
    });

    this.getTodosHandler = async () => [];

    this.updateTodoHandler = async () => ({
      success: false,
      error: runtimeUnavailable,
    });

    this.claimNextTodoHandler = async () => ({
      claimed: false,
      remaining: 0,
      error: runtimeUnavailable,
    });

    this.compactContextHandler = async () => ({
      success: false,
      compressed: false,
      error: runtimeUnavailable,
    });
  }

  /**
   * 设置可用 Worker 列表（由 MissionDrivenEngine 从 ProfileLoader 注入）
   */
  setAvailableWorkers(workers: { slot: WorkerSlot; description: string }[]): void {
    // 必须无条件覆盖，避免”全禁用后仍保留旧枚举”的陈旧状态
    this.availableWorkers = workers;
  }

  private getWorkerEnum(): string[] {
    if (this.availableWorkers.length === 0) {
      logger.warn('OrchestrationExecutor.getWorkerEnum: Worker 列表未注入，使用空列表', undefined, LogCategory.TOOLS);
    }
    return this.availableWorkers.map(w => w.slot);
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
    claimNextTodo?: ClaimNextTodoHandler;
    compactContext?: CompactContextHandler;
  }): void {
    if (handlers.dispatch) this.dispatchHandler = handlers.dispatch;
    if (handlers.sendMessage) this.sendMessageHandler = handlers.sendMessage;
    if (handlers.waitForWorkers) this.waitForWorkersHandler = handlers.waitForWorkers;
    if (handlers.splitTodo) this.splitTodoHandler = handlers.splitTodo;
    if (handlers.getTodos) this.getTodosHandler = handlers.getTodos;
    if (handlers.updateTodo) this.updateTodoHandler = handlers.updateTodo;
    if (handlers.claimNextTodo) this.claimNextTodoHandler = handlers.claimNextTodo;
    if (handlers.compactContext) this.compactContextHandler = handlers.compactContext;
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
      this.getCompactContextDefinition(),
      this.getClaimNextTodoDefinition(),
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
      case 'worker_dispatch':
        return this.executeDispatchTask(toolCall);
      case 'worker_wait':
        return this.executeWaitForWorkers(toolCall);
      case 'worker_send_message':
        return this.executeSendWorkerMessage(toolCall);
      case 'todo_split':
        return this.executeSplitTodo(toolCall, callerContext);
      case 'todo_list':
        return this.executeGetTodos(toolCall, callerContext);
      case 'todo_update':
        return this.executeUpdateTodo(toolCall);
      case 'context_compact':
        return this.executeCompactContext(toolCall);
      case 'todo_claim_next':
        return this.executeClaimNextTodo(toolCall, callerContext);
      default:
        return {
          toolCallId: toolCall.id,
          content: `Unknown orchestration tool: ${toolCall.name}`,
          isError: true,
        };
    }
  }

  // ===========================================================================
  // worker_dispatch
  // ===========================================================================

  private getDispatchTaskDefinition(): ExtendedToolDefinition {
    const taskItemProperties: Record<string, any> = {
      task_name: {
        type: 'string',
        description: '标准的工程化任务名称，简短概括任务内容（例如：重构用户登录模块，修复导航栏溢出 Bug 等），不要照抄用户原始对话。'
      },
      ownership_hint: {
        type: 'string',
        enum: ['frontend', 'backend', 'integration', 'data_analysis', 'general', 'auto'],
        description: '任务归属域（决定由哪个 Worker 执行）。frontend=前端UI/组件/样式，backend=后端API/数据库/服务端，integration=跨端联调/集成，data_analysis=数据分析/可视化，general=通用任务，auto=由系统从任务文本自动推断。若一个功能同时涉及 frontend/backend 等多个职责域，必须先拆成多个任务再分别派发。',
      },
      mode_hint: {
        type: 'string',
        enum: ['implement', 'test', 'document', 'review', 'debug', 'refactor', 'architecture', 'auto'],
        description: '任务执行模式（约束 Worker 的执行行为，不影响路由）。implement=功能实现（默认），test=编写/修复测试，document=编写文档，review=代码审查，debug=调试排查，refactor=重构，architecture=架构设计，auto=由系统从任务文本自动推断。',
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
      idempotency_key: {
        type: 'string',
        description: '可选幂等键。相同键在同一会话/任务上下文下会被判定为同一次派发，避免重复执行。',
      },
    };

    return {
      name: 'worker_dispatch',
      description: '向一个或多个专业 AI Worker 派发任务。每个任务通过 ownership_hint 指定归属域（frontend/backend/integration 等，决定哪个 Worker 执行），通过 mode_hint 指定执行模式（implement/test/debug 等，约束 Worker 行为）。若一个功能同时涉及 frontend/backend 等多个职责域，必须先拆成多个任务，不能用单个泛化任务整包派发。',
      input_schema: {
        type: 'object',
        properties: {
          mission_title: {
            type: 'string',
            description: '【首次 dispatch 必填】本轮任务的整体标题，简短概括核心目标（如"集成管理后台前端页面"、"修复用户登录流程 Bug"）。不要照抄用户原始消息，必须重新概括为规范的工程化标题。后续 worker_dispatch 调用可省略。',
          },
          tasks: {
            type: 'array',
            description: '待派发的任务列表（至少 1 个）。每个任务将独立路由到对应 Worker 并行执行。每个任务应尽量保持单一归属域；跨职责域功能应先拆分为多个任务。',
            items: {
              type: 'object',
              properties: taskItemProperties,
              required: ['task_name', 'ownership_hint', 'mode_hint', 'goal', 'acceptance', 'constraints', 'context', 'requires_modification'],
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
    fallbackIdempotencyKey: string,
  ): { ok: true; params: Parameters<DispatchTaskHandler>[0] } | { ok: false; error: string } {
    const prefix = `tasks[${index}]`;

    if (!task.task_name || typeof task.task_name !== 'string' || !task.task_name.trim()) {
      return { ok: false, error: `${prefix}: task_name 是必填参数` };
    }

    // ownership_hint + mode_hint 验证：只接受新协议字段
    let ownershipHint = typeof task.ownership_hint === 'string' ? task.ownership_hint.trim() : '';
    let modeHint = typeof task.mode_hint === 'string' ? task.mode_hint.trim() : '';

    if (typeof task.category === 'string' && task.category.trim()) {
      return { ok: false, error: `${prefix}: category 已废弃，必须改用 ownership_hint + mode_hint` };
    }

    if (!ownershipHint) ownershipHint = 'auto';
    if (!modeHint) modeHint = 'auto';

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
    const rawIdempotencyKey = typeof task.idempotency_key === 'string' ? task.idempotency_key.trim() : '';
    const idempotencyKey = rawIdempotencyKey || fallbackIdempotencyKey;

    return {
      ok: true,
      params: {
        worker: 'auto',
        ownershipHint,
        modeHint,
        idempotencyKey,
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
    const normalizedTasks = this.normalizeDispatchTasks(toolCall.arguments);
    if (!normalizedTasks.ok) {
      return {
        toolCallId: toolCall.id,
        content: `Error: ${normalizedTasks.error}`,
        isError: true,
      };
    }
    const tasks = normalizedTasks.tasks;

    // 提取顶层 mission_title（由编排者在首次 dispatch 时提供）
    const rawArgs = toolCall.arguments as Record<string, unknown> | undefined;
    const missionTitle = typeof rawArgs?.mission_title === 'string' ? rawArgs.mission_title.trim() : '';

    // 阶段 1：全量验证（任一任务校验失败则整批拒绝，避免部分派发）
    const validatedParams: Parameters<DispatchTaskHandler>[0][] = [];
    for (let i = 0; i < tasks.length; i++) {
      const fallbackIdempotencyKey = `${toolCall.id}:${i}`;
      const validation = this.validateSingleTaskArgs(tasks[i], i, fallbackIdempotencyKey);
      if (!validation.ok) {
        return {
          toolCallId: toolCall.id,
          content: `Error: ${validation.error}`,
          isError: true,
        };
      }
      validatedParams.push(validation.params);
    }

    // 将 mission_title 注入第一个任务的参数中，由 handler 链路传递给引擎
    if (missionTitle && validatedParams.length > 0) {
      validatedParams[0].missionTitle = missionTitle;
    }

    logger.info('worker_dispatch 开始批量派发', {
      taskCount: validatedParams.length,
      ownerships: validatedParams.map(p => p.ownershipHint),
      modes: validatedParams.map(p => p.modeHint),
      taskNames: validatedParams.map(p => p.task_name),
      missionTitle: missionTitle || undefined,
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
      logger.error('worker_dispatch 批量执行失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `worker_dispatch failed: ${error.message}`,
        isError: true,
      };
    }
  }

  private normalizeDispatchTasks(
    rawArguments: unknown,
  ): { ok: true; tasks: Record<string, any>[] } | { ok: false; error: string } {
    if (!rawArguments || typeof rawArguments !== 'object' || Array.isArray(rawArguments)) {
      return { ok: false, error: 'worker_dispatch 参数必须是对象，且包含 tasks 字段' };
    }

    const args = rawArguments as { tasks?: unknown };
    const rawTasks = args.tasks;

    if (Array.isArray(rawTasks)) {
      if (rawTasks.length === 0) {
        return { ok: false, error: 'tasks 必须是至少包含 1 个元素的数组' };
      }
      return { ok: true, tasks: rawTasks as Record<string, any>[] };
    }

    if (typeof rawTasks === 'string') {
      const trimmed = rawTasks.trim();
      if (!trimmed) {
        return { ok: false, error: 'tasks 不能为空字符串，必须是任务数组' };
      }

      // 尝试解析 JSON 字符串为数组（先直接解析，失败后修复再试）
      const candidates = [trimmed];
      const repaired = repairJSON(trimmed);
      if (repaired !== trimmed) {
        candidates.push(repaired);
      }

      let lastParseError: string = '';
      for (const candidate of candidates) {
        try {
          const parsed = JSON.parse(candidate) as unknown;
          if (Array.isArray(parsed)) {
            if (parsed.length === 0) {
              return { ok: false, error: 'tasks 必须是至少包含 1 个元素的数组' };
            }
            if (candidate !== trimmed) {
              logger.warn('worker_dispatch: tasks JSON 修复后解析成功', {
                taskCount: parsed.length,
              }, LogCategory.TOOLS);
            } else {
              logger.warn('worker_dispatch 参数归一化：tasks 从 JSON 字符串解析为数组', {
                taskCount: parsed.length,
              }, LogCategory.TOOLS);
            }
            return { ok: true, tasks: parsed as Record<string, any>[] };
          }
          return { ok: false, error: 'tasks 字符串解析后不是数组，请传递任务数组' };
        } catch (error: any) {
          lastParseError = error?.message || String(error);
        }
      }

      return {
        ok: false,
        error: `tasks 不是有效 JSON 数组字符串: ${lastParseError}`,
      };
    }

    return { ok: false, error: 'tasks 必须是至少包含 1 个元素的数组' };
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

    // 容错：过滤非字符串元素（LLM 可能混入 null/number），不因个别脏元素拒绝整批任务
    const normalized = raw
      .filter((item): item is string => typeof item === 'string')
      .map(item => item.trim())
      .filter(Boolean);

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
  // worker_wait
  // ===========================================================================

  private getWaitForWorkersDefinition(): ExtendedToolDefinition {
    return {
      name: 'worker_wait',
      description: '等待已分配的 Worker 完成执行并返回结果。这是反应式编排的核心工具：worker_dispatch 发送任务后，调用此工具阻塞等待结果，然后根据结果决定是否追加新任务或结束。不传 task_ids 则等待当前批次全部完成。返回包含 wait_status（completed/timeout）和 pending_task_ids，timeout 时必须继续决策，不可当作全部完成。全量完成时额外返回 audit（程序化审计结论）。',
      input_schema: {
        type: 'object',
        properties: {
          task_ids: {
            type: 'array',
            items: { type: 'string' },
            description: '等待的 task_id 列表（由 worker_dispatch 返回）。不传则等待当前批次中所有任务完成',
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
    const args = toolCall.arguments as { task_ids?: string[] };

    logger.info('worker_wait 开始等待', {
      taskIds: args.task_ids || 'all',
    }, LogCategory.TOOLS);

    try {
      const result = await this.waitForWorkersHandler({
        task_ids: args.task_ids,
      });

      logger.info('worker_wait 完成', {
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
      logger.error('worker_wait 失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `worker_wait failed: ${error.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // worker_send_message
  // ===========================================================================

  private getSendWorkerMessageDefinition(): ExtendedToolDefinition {
    return {
      name: 'worker_send_message',
      description: '向指定 Worker 发送运行时指令，并同步展示在 Worker 面板。用于传递补充上下文、调整指令或协作信息。',
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

    logger.info('worker_send_message', {
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
      logger.error('worker_send_message 失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `worker_send_message failed: ${error.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // todo_split
  // ===========================================================================

  private getSplitTodoDefinition(): ExtendedToolDefinition {
    return {
      name: 'todo_split',
      description: '将当前任务拆分为多个子步骤。当任务包含多个可独立完成和验证的子目标时使用。每个子步骤都必须给出明确的预期产出；若已知目标文件，也必须写清。todo_split 只能细化当前 Assignment 内的执行步骤，不能借此把任务重新拆到其他 ownership 域或替代 orchestrator 级的跨 Worker Assignment 拆分。拆分后每个子步骤将依次执行，全部完成后父任务自动标记完成。',
      input_schema: {
        type: 'object',
        properties: {
          subtasks: {
            type: 'array',
            minItems: 2,
            maxItems: 8,
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
                expected_output: {
                  type: 'string',
                  description: '该子步骤完成时必须交付的明确结果，用于执行收敛与验收判断',
                },
                target_files: {
                  type: 'array',
                  items: { type: 'string' },
                  description: '若已知此子步骤主要涉及哪些文件，请填写目标文件列表',
                },
                type: {
                  type: 'string',
                  enum: ['implementation', 'verification', 'discovery'],
                  description: '子步骤类型：implementation(实现)、verification(验证)、discovery(探索/分析)',
                },
              },
              required: ['content', 'reasoning', 'expected_output', 'type'],
            },
            description: '子步骤列表（2-8 个）。禁止把连续动作机械切碎；只拆分真正可独立完成和验证的子目标。子步骤必须继续服务当前 Assignment 的同一 ownership，不得把前端/后端等跨域工作混入子步骤。',
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
    if (!callerContext) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: todo_split 需要执行上下文（仅 Worker 可调用）',
        isError: true,
      };
    }

    const args = toolCall.arguments as {
      subtasks?: Array<{
        content?: string;
        reasoning?: string;
        expected_output?: string;
        target_files?: string[];
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
    if (args.subtasks.length > 8) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: subtasks 最多允许 8 个，避免把连续动作机械切碎',
        isError: true,
      };
    }

    const validTypes = ['implementation', 'verification', 'discovery'];
    const normalizedContents = new Set<string>();
    for (const subtask of args.subtasks) {
      if (!subtask.content || typeof subtask.content !== 'string' || !subtask.content.trim()) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: 每个子步骤必须有非空的 content',
          isError: true,
        };
      }
      const normalizedContent = subtask.content.trim().toLowerCase();
      if (normalizedContents.has(normalizedContent)) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: 子步骤 content 不可重复，请重新收敛拆分边界',
          isError: true,
        };
      }
      normalizedContents.add(normalizedContent);
      if (!subtask.reasoning || typeof subtask.reasoning !== 'string') {
        return {
          toolCallId: toolCall.id,
          content: 'Error: 每个子步骤必须有 reasoning',
          isError: true,
        };
      }
      if (!subtask.expected_output || typeof subtask.expected_output !== 'string' || !subtask.expected_output.trim()) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: 每个子步骤必须有非空的 expected_output',
          isError: true,
        };
      }
      if (subtask.target_files !== undefined && !Array.isArray(subtask.target_files)) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: target_files 必须是字符串数组',
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

    logger.info('todo_split 开始', {
      todoId: callerContext.todoId,
      subtaskCount: args.subtasks.length,
      workerId: callerContext.workerId,
    }, LogCategory.TOOLS);

    try {
      const result = await this.splitTodoHandler({
        subtasks: args.subtasks.map(s => ({
          content: s.content!.trim(),
          reasoning: s.reasoning!.trim(),
          expectedOutput: s.expected_output!.trim(),
          targetFiles: Array.isArray(s.target_files)
            ? s.target_files
              .filter((item): item is string => typeof item === 'string')
              .map((item) => item.trim())
              .filter(Boolean)
            : undefined,
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
      logger.error('todo_split 执行失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `todo_split failed: ${error.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // todo_list
  // ===========================================================================

  private getGetTodosDefinition(): ExtendedToolDefinition {
    return {
      name: 'todo_list',
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
        parentId: t.parentId,
        content: t.content,
        source: t.source,
        status: t.status,
        worker: t.workerId,
        blockedReason: t.blockedReason,
        approvalStatus: t.approvalStatus,
        dependsOn: t.dependsOn,
        required: (t as any).required ?? true,
        effortWeight: (t as any).effortWeight ?? 1,
        waiverApproved: (t as any).waiverApproved ?? false,
        createdAt: t.createdAt,
      }));
      this.lastTodoSnapshot = {
        ids: new Set(summary.map(item => item.id).filter(id => typeof id === 'string' && id.trim().length > 0)),
        total: summary.length,
        updatedAt: Date.now(),
        scope: {
          missionId: typeof args.mission_id === 'string' ? args.mission_id : undefined,
          sessionId: typeof args.session_id === 'string' ? args.session_id : undefined,
        },
      };
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
  // todo_update
  // ===========================================================================

  private getUpdateTodoDefinition(): ExtendedToolDefinition {
    return {
      name: 'todo_update',
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
                content: { type: 'string', description: '更新任务描述' },
                force_reset: { type: 'boolean', description: '当 status=pending 且 Todo 正在运行时，强制中断并重置为 pending。' }
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
    const args = toolCall.arguments as {
      updates: Array<{ todo_id: string; status?: UpdateTodoStatus | string; content?: string; force_reset?: boolean }>;
    };
    try {
      if (!args.updates || !Array.isArray(args.updates)) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: updates 必须是数组',
          isError: true,
        };
      }

      if (!this.lastTodoSnapshot || this.lastTodoSnapshot.total === 0) {
        return {
          toolCallId: toolCall.id,
          content: 'Error: 当前没有可更新的 Todo，请先使用 todo_list 获取列表',
          isError: true,
        };
      }

      for (const update of args.updates) {
        if (!update.todo_id || !this.lastTodoSnapshot.ids.has(update.todo_id)) {
          return {
            toolCallId: toolCall.id,
            content: `Error: todo_id=${update.todo_id || ''} 不在最近的 Todo 列表中，请先使用 todo_list 刷新`,
            isError: true,
          };
        }
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
        content: u.content,
        forceReset: u.force_reset === true,
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

  // ===========================================================================
  // context_compact（手动上下文压缩）
  // ===========================================================================

  private getCompactContextDefinition(): ExtendedToolDefinition {
    return {
      name: 'context_compact',
      description: '手动触发会话上下文压缩与归档。适用于长会话中需要主动控制上下文预算时，由编排者显式调用。',
      input_schema: {
        type: 'object',
        properties: {
          force: {
            type: 'boolean',
            description: '是否强制压缩（即使当前未达到自动压缩阈值）',
          },
          reason: {
            type: 'string',
            description: '可选：本次手动压缩的备注原因（用于归档审计）',
          },
        },
        required: [],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'context', 'compact', 'governance'],
      },
    };
  }

  private async executeCompactContext(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as { force?: boolean; reason?: string };
    const force = args?.force === true;
    const reason = typeof args?.reason === 'string' ? args.reason.trim() : '';
    try {
      const result = await this.compactContextHandler({
        force,
        reason: reason || undefined,
      });
      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(result),
        isError: !result.success,
      };
    } catch (err: any) {
      return {
        toolCallId: toolCall.id,
        content: `context_compact failed: ${err.message}`,
        isError: true,
      };
    }
  }

  // ===========================================================================
  // todo_claim_next（半自治 Worker 主动认领任务）
  // ===========================================================================

  private getClaimNextTodoDefinition(): ExtendedToolDefinition {
    return {
      name: 'todo_claim_next',
      description:
        '主动查询并认领当前 Mission 中下一个可执行的 Todo。' +
        '当前 Todo 完成后，调用此工具继续认领与当前上下文足够接近的下一个任务，无需等待 Orchestrator 重新分配。' +
        '系统只会自动续领同一 Assignment 或共享目标文件的 Todo；若不存在足够亲和的任务，将返回 claimed=false。' +
        '如果没有可认领的 Todo，将返回 claimed=false。',
      input_schema: {
        type: 'object',
        properties: {},
        required: [],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'worker', 'autonomous', 'claim'],
      },
    };
  }

  private async executeClaimNextTodo(
    toolCall: ToolCall,
    callerContext?: SplitTodoCallerContext
  ): Promise<ToolResult> {
    if (!callerContext) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: todo_claim_next 需要执行上下文（仅 Worker 可调用）',
        isError: true,
      };
    }

    logger.info('todo_claim_next 开始', {
      missionId: callerContext.missionId,
      workerId: callerContext.workerId,
    }, LogCategory.TOOLS);

    try {
      const result = await this.claimNextTodoHandler({
        missionId: callerContext.missionId,
        workerId: callerContext.workerId,
        callerContext,
      });

      if (result.claimed && result.todo) {
        logger.info('todo_claim_next 认领成功', {
          todoId: result.todo.id,
          content: result.todo.content.substring(0, 80),
          affinity: result.affinity?.level,
          remaining: result.remaining,
        }, LogCategory.TOOLS);
      } else {
        logger.info('todo_claim_next 无可认领任务', {
          remaining: result.remaining,
          reason: result.reason,
          error: result.error,
        }, LogCategory.TOOLS);
      }

      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(result),
        isError: false,
      };
    } catch (err: any) {
      logger.error('todo_claim_next 执行失败', { error: err.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `todo_claim_next failed: ${err.message}`,
        isError: true,
      };
    }
  }
}
