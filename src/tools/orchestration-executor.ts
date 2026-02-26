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
 * dispatch_task 回调：由 MissionDrivenEngine 注入，实际执行 Worker 委派
 * 返回 task_id 后立即结束（非阻塞），Worker 在后台异步执行
 */
export type DispatchTaskHandler = (params: {
  worker: 'auto';
  category: string;
  /** 是否要求子任务对目标文件产生实际修改 */
  requiresModification: boolean;
  task: string;
  files?: string[];
  dependsOn?: string[];
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
}

/**
 * wait_for_workers 回调：阻塞直到指定（或全部）Worker 完成
 */
export type WaitForWorkersHandler = (params: {
  task_ids?: string[];
}) => Promise<WaitForWorkersResult>;

/**
 * 编排工具执行器
 */
export class OrchestrationExecutor {
  private dispatchHandler?: DispatchTaskHandler;
  private sendMessageHandler?: SendWorkerMessageHandler;
  private waitForWorkersHandler?: WaitForWorkersHandler;
  /** 动态 Worker 列表（必须由 MissionDrivenEngine 从 ProfileLoader 注入） */
  private availableWorkers: { slot: WorkerSlot; description: string }[] = [];
  /** Category → Worker 映射（必须由 DispatchManager 从 ProfileLoader 注入） */
  private categoryWorkerMap: CategoryWorkerEntry[] = [];

  private static readonly TOOL_NAMES = ['dispatch_task', 'send_worker_message', 'wait_for_workers'] as const;

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
  }): void {
    this.dispatchHandler = handlers.dispatch;
    this.sendMessageHandler = handlers.sendMessage;
    this.waitForWorkersHandler = handlers.waitForWorkers;
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
    ];
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
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
          task: {
            type: 'string',
            description: '清晰、完整的任务描述，包含目标、约束和验收标准',
          },
          requires_modification: {
            type: 'boolean',
            description: '是否要求该任务对目标文件产生实际修改。只读分析/统计/总结任务必须传 false；功能开发/修复/重构任务传 true。',
          },
          files: {
            type: 'array',
            items: { type: 'string' },
            description: '任务涉及的关键文件路径，相对于工作区根目录（可选，帮助 Worker 定位。例如 "src/tools/search-executor.ts"）',
          },
          depends_on: {
            type: 'array',
            items: { type: 'string' },
            description: '依赖的前序任务 task_id 列表。被依赖的任务完成后本任务才会执行，可通过 SharedContextPool 获取前序任务的输出上下文',
          },
        },
        required: ['category', 'task', 'requires_modification'],
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
      task?: string;
      requires_modification?: boolean;
      files?: string[];
      depends_on?: string[];
    };

    // category 和 task 是必填参数
    if (!args.category || typeof args.category !== 'string' || !args.category.trim()) {
      const validCategories = this.getCategoryEnum();
      return {
        toolCallId: toolCall.id,
        content: `Error: category 是必填参数。可选值: ${validCategories.join(', ')}`,
        isError: true,
      };
    }
    if (!args.task || typeof args.task !== 'string' || !args.task.trim()) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: task 是必填参数，需包含明确的目标、文件路径和验收标准',
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
      taskPreview: args.task.substring(0, 80),
      files: args.files,
      dependsOn: args.depends_on,
    }, LogCategory.TOOLS);

    try {
      const result = await this.dispatchHandler({
        worker: 'auto',
        category,
        requiresModification: args.requires_modification,
        task: args.task,
        files: args.files,
        dependsOn: args.depends_on,
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

  // ===========================================================================
  // wait_for_workers
  // ===========================================================================

  private getWaitForWorkersDefinition(): ExtendedToolDefinition {
    return {
      name: 'wait_for_workers',
      description: '等待已分配的 Worker 完成执行并返回结果。这是反应式编排的核心工具：dispatch_task 发送任务后，调用此工具阻塞等待结果，然后根据结果决定是否追加新任务或结束。不传 task_ids 则等待当前批次全部完成。返回包含 wait_status（completed/timeout）和 pending_task_ids，timeout 时必须继续决策，不可当作全部完成。',
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
}
