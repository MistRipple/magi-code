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
 * dispatch_task 回调：由 MissionDrivenEngine 注入，实际执行 Worker 委派
 * 返回 task_id 后立即结束（非阻塞），Worker 在后台异步执行
 */
export type DispatchTaskHandler = (params: {
  worker: WorkerSlot;
  task: string;
  files?: string[];
  dependsOn?: string[];
}) => Promise<{
  task_id: string;
  status: 'dispatched' | 'failed';
  worker: WorkerSlot;
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
 * wait_for_workers 回调：阻塞直到指定（或全部）Worker 完成
 */
export type WaitForWorkersHandler = (params: {
  task_ids?: string[];
}) => Promise<{
  results: WorkerCompletionResult[];
}>;

/**
 * 编排工具执行器
 */
export class OrchestrationExecutor {
  private dispatchHandler?: DispatchTaskHandler;
  private sendMessageHandler?: SendWorkerMessageHandler;
  private waitForWorkersHandler?: WaitForWorkersHandler;
  /** 动态 Worker 列表（必须由 MissionDrivenEngine 从 ProfileLoader 注入） */
  private availableWorkers: { slot: WorkerSlot; description: string }[] = [];

  private static readonly TOOL_NAMES = ['dispatch_task', 'send_worker_message', 'wait_for_workers'] as const;

  /**
   * 设置可用 Worker 列表（由 MissionDrivenEngine 从 ProfileLoader 注入）
   */
  setAvailableWorkers(workers: { slot: WorkerSlot; description: string }[]): void {
    // 必须无条件覆盖，避免“全禁用后仍保留旧枚举”的陈旧状态
    this.availableWorkers = workers;
  }

  private getWorkerEnum(): string[] {
    if (this.availableWorkers.length === 0) {
      logger.warn('OrchestrationExecutor.getWorkerEnum: Worker 列表未注入，使用空列表', undefined, LogCategory.TOOLS);
    }
    return this.availableWorkers.map(w => w.slot);
  }

  private getWorkerDescription(): string {
    return this.availableWorkers.map(w => `${w.slot}: ${w.description}`).join('；');
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
    return {
      name: 'dispatch_task',
      description: '将子任务分配给专业 AI Worker 执行。适用于需要多步代码操作、多文件修改或专业领域知识的任务。Worker 将自主完成任务并在主对话区回传执行进度和结果。',
      input_schema: {
        type: 'object',
        properties: {
          worker: {
            type: 'string',
            enum: this.getWorkerEnum(),
            description: `目标 Worker。${this.getWorkerDescription()}`,
          },
          task: {
            type: 'string',
            description: '清晰、完整的任务描述，包含目标、约束和验收标准',
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
        required: ['worker', 'task'],
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

    const args = toolCall.arguments as { worker: string; task: string; files?: string[]; depends_on?: string[] };

    if (!args.worker || !args.task) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: worker and task are required',
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

    logger.info('dispatch_task 开始', {
      worker: args.worker,
      taskPreview: args.task.substring(0, 80),
      files: args.files,
      dependsOn: args.depends_on,
    }, LogCategory.TOOLS);

    try {
      const result = await this.dispatchHandler({
        worker: args.worker as WorkerSlot,
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
      description: '等待已分配的 Worker 完成执行并返回结果。这是反应式编排的核心工具：dispatch_task 发送任务后，调用此工具阻塞等待结果，然后根据结果决定是否追加新任务或结束。不传 task_ids 则等待当前批次全部完成。',
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
