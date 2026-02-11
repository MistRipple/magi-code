/**
 * DirectExecutionService - 直接 Worker 执行服务
 *
 * 从 WebviewProvider 提取的业务逻辑（P1-1 修复）。
 * 职责：管理直接 Worker 执行模式的完整生命周期。
 */

import { logger, LogCategory } from '../logging';
import { isAbortError } from '../errors';
import type { WorkerSlot } from '../types/agent-types';
import type { ToolManager } from '../tools/tool-manager';
import type { ExecutionRecord } from '../orchestrator/execution-stats';

export interface DirectExecutionResult {
  success: boolean;
  error?: string;
}

/**
 * DirectExecutionService 依赖接口
 * 通过依赖注入避免直接引用 WebviewProvider 的内部状态
 */
export interface DirectExecutionDeps {
  getSessionId: () => string;
  getToolManager: () => ToolManager;
  sendMessage: (worker: WorkerSlot, prompt: string, imagePaths: string[]) => Promise<{
    content?: string;
    error?: string;
    tokenUsage?: { inputTokens?: number; outputTokens?: number };
  }>;
  // 任务生命周期
  createTaskFromPrompt: (sessionId: string, prompt: string) => Promise<{ id: string }>;
  markTaskExecuting: (taskId: string) => Promise<void>;
  completeTaskById: (taskId: string) => Promise<void>;
  failTaskById: (taskId: string, error: string) => Promise<void>;
  cancelTaskById: (taskId: string) => Promise<void>;
  getExecutionStats: () => { recordExecution: (record: Omit<ExecutionRecord, 'timestamp'>) => void } | null;
  // UI 通知
  sendStateUpdate: () => void;
  sendErrorMessage: (content: string, worker: WorkerSlot) => void;
  sendResultMessage: (content: string, worker: WorkerSlot) => void;
  saveMessageToSession: (prompt: string, content: string, worker: WorkerSlot) => void;
}

export class DirectExecutionService {
  constructor(private deps: DirectExecutionDeps) {}

  /**
   * 执行直接 Worker 模式
   */
  async execute(
    prompt: string,
    targetWorker: WorkerSlot,
    imagePaths: string[],
  ): Promise<DirectExecutionResult> {
    logger.info('界面.执行.模式.直接', { agent: targetWorker }, LogCategory.UI);

    const startTime = Date.now();
    const sessionId = this.deps.getSessionId();
    const task = await this.deps.createTaskFromPrompt(sessionId, prompt);
    await this.deps.markTaskExecuting(task.id);
    this.deps.sendStateUpdate();

    let errorMsg: string | undefined;
    let success = false;
    const toolManager = this.deps.getToolManager();

    try {
      // 设置快照上下文（直接 Worker 模式也需要精确记录文件变更）
      toolManager.setSnapshotContext({
        missionId: task.id,
        assignmentId: `direct-${task.id}`,
        todoId: `direct-${task.id}`,
        workerId: targetWorker,
      });

      logger.info('界面.执行.直接.请求', { worker: targetWorker }, LogCategory.UI);
      const response = await this.deps.sendMessage(targetWorker, prompt, imagePaths);
      logger.info('界面.执行.直接.响应', { worker: targetWorker, preview: response.content?.substring(0, 100) }, LogCategory.UI);

      const executionStats = this.deps.getExecutionStats();
      if (executionStats) {
        executionStats.recordExecution({
          worker: targetWorker,
          taskId: task.id,
          subTaskId: `direct-${task.id}`,
          success: !response.error,
          duration: Date.now() - startTime,
          error: response.error,
          inputTokens: response.tokenUsage?.inputTokens,
          outputTokens: response.tokenUsage?.outputTokens,
        });
      }

      if (response.error) {
        errorMsg = response.error;
        await this.deps.failTaskById(task.id, response.error);
        this.deps.sendErrorMessage(
          `${targetWorker.toUpperCase()}: ${response.error}`,
          targetWorker,
        );
      } else {
        await this.deps.completeTaskById(task.id);
        this.deps.saveMessageToSession(prompt, response.content || '', targetWorker);
        this.deps.sendResultMessage(
          `${targetWorker.toUpperCase()} 已完成任务。详细内容请查看 ${targetWorker.toUpperCase()} 面板。`,
          targetWorker,
        );
        success = true;
      }
    } catch (error) {
      // 中断导致的 abort 错误静默处理
      if (isAbortError(error)) {
        logger.info('界面.执行.直接.中断', { worker: targetWorker }, LogCategory.UI);
        await this.deps.cancelTaskById(task.id);
      } else {
        logger.error('界面.执行.直接.失败', error, LogCategory.UI);
        errorMsg = error instanceof Error ? error.message : String(error);
        await this.deps.failTaskById(task.id, errorMsg);
        const executionStats = this.deps.getExecutionStats();
        if (executionStats) {
          executionStats.recordExecution({
            worker: targetWorker,
            taskId: task.id,
            subTaskId: `direct-${task.id}`,
            success: false,
            duration: Date.now() - startTime,
            error: errorMsg,
          });
        }
        this.deps.sendErrorMessage(
          `${targetWorker.toUpperCase()}: ${errorMsg}`,
          targetWorker,
        );
      }
    } finally {
      toolManager.clearSnapshotContext(targetWorker);
    }

    this.deps.sendStateUpdate();
    return success ? { success: true } : { success: false, error: errorMsg };
  }
}
