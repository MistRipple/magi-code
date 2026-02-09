/**
 * 编排工具执行器
 * 提供 dispatch_task、plan_mission、send_worker_message 三个元工具
 *
 * 这些工具使 orchestrator LLM 能够：
 * - dispatch_task: 将子任务分配给专业 Worker 执行
 * - plan_mission: 为复杂多 Worker 任务创建协作执行计划
 * - send_worker_message: 向 Worker 面板发送消息
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
 * plan_mission 回调：由 MissionDrivenEngine 注入，创建 Mission 并执行规划
 */
export type PlanMissionHandler = (params: {
  goal: string;
  constraints?: string[];
  workers?: WorkerSlot[];
}) => Promise<{
  success: boolean;
  missionId?: string;
  summary: string;
  errors?: string[];
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
 * 编排工具执行器
 */
export class OrchestrationExecutor {
  private dispatchHandler?: DispatchTaskHandler;
  private planHandler?: PlanMissionHandler;
  private sendMessageHandler?: SendWorkerMessageHandler;
  /** 动态 Worker 列表（必须由 MissionDrivenEngine 从 ProfileLoader 注入） */
  private availableWorkers: { slot: WorkerSlot; description: string }[] = [];

  private static readonly TOOL_NAMES = ['dispatch_task', 'plan_mission', 'send_worker_message'] as const;

  /**
   * 设置可用 Worker 列表（由 MissionDrivenEngine 从 ProfileLoader 注入）
   */
  setAvailableWorkers(workers: { slot: WorkerSlot; description: string }[]): void {
    if (workers.length > 0) {
      this.availableWorkers = workers;
    }
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
    plan?: PlanMissionHandler;
    sendMessage?: SendWorkerMessageHandler;
  }): void {
    this.dispatchHandler = handlers.dispatch;
    this.planHandler = handlers.plan;
    this.sendMessageHandler = handlers.sendMessage;
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
      this.getPlanMissionDefinition(),
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
      case 'plan_mission':
        return this.executePlanMission(toolCall);
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
  // plan_mission
  // ===========================================================================

  private getPlanMissionDefinition(): ExtendedToolDefinition {
    return {
      name: 'plan_mission',
      description: '为复杂的多步骤任务创建协作执行计划。适用于需要多个 Worker 协作、涉及架构变更、或需要用户审批的重大任务。会生成详细的任务分解、Worker 间协作契约和验收标准，并征求用户确认。',
      input_schema: {
        type: 'object',
        properties: {
          goal: {
            type: 'string',
            description: '任务目标的完整描述',
          },
          constraints: {
            type: 'array',
            items: { type: 'string' },
            description: '约束条件',
          },
          workers: {
            type: 'array',
            items: {
              type: 'string',
              enum: this.getWorkerEnum(),
            },
            description: '建议参与的 Worker',
          },
        },
        required: ['goal'],
      },
      metadata: {
        source: 'builtin',
        category: 'orchestration',
        tags: ['orchestration', 'mission', 'planning'],
      },
    };
  }

  private async executePlanMission(toolCall: ToolCall): Promise<ToolResult> {
    if (!this.planHandler) {
      return {
        toolCallId: toolCall.id,
        content: 'plan_mission handler not configured',
        isError: true,
      };
    }

    const args = toolCall.arguments as { goal: string; constraints?: string[]; workers?: string[] };

    if (!args.goal) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: goal is required',
        isError: true,
      };
    }

    logger.info('plan_mission 开始', {
      goalPreview: args.goal.substring(0, 80),
      constraints: args.constraints,
      workers: args.workers,
    }, LogCategory.TOOLS);

    try {
      const result = await this.planHandler({
        goal: args.goal,
        constraints: args.constraints,
        workers: args.workers as WorkerSlot[] | undefined,
      });

      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(result),
        isError: !result.success,
      };
    } catch (error: any) {
      logger.error('plan_mission 执行失败', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `plan_mission failed: ${error.message}`,
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
