/**
 * Orchestrator - 核心编排器
 * 负责任务分解、Worker 调度、结果收集
 */

import { CLIType, Task, SubTask, TaskCategory, WorkerResult, ExecutionMode } from './types';
import { UnifiedSessionManager } from './session';
import { TaskManager } from './task-manager';
import { SnapshotManager } from './snapshot-manager';
import { CLIDetector } from './cli-detector';
import { BaseWorker } from './workers/base-worker';
import { createClaudeWorker } from './workers/claude-worker';
import { createCodexWorker } from './workers/codex-worker';
import { createGeminiWorker } from './workers/gemini-worker';
import { globalEventBus } from './events';

/** Orchestrator 配置 */
export interface OrchestratorOptions {
  workspaceRoot: string;
  sessionManager: UnifiedSessionManager;
  taskManager: TaskManager;
  snapshotManager: SnapshotManager;
  mode?: ExecutionMode;
  timeout?: number;
}

/**
 * Orchestrator 编排器
 */
export class Orchestrator {
  private options: OrchestratorOptions;
  private cliDetector: CLIDetector;
  private workers: Map<CLIType, BaseWorker> = new Map();
  private isRunning = false;

  constructor(options: OrchestratorOptions) {
    this.options = options;
    this.cliDetector = new CLIDetector();
    this.initWorkers();
  }

  /** 初始化 Workers */
  private initWorkers(): void {
    const { workspaceRoot, timeout = 300000 } = this.options;
    this.workers.set('claude', createClaudeWorker('claude', workspaceRoot, timeout));
    this.workers.set('codex', createCodexWorker('codex', workspaceRoot, timeout));
    this.workers.set('gemini', createGeminiWorker('gemini', workspaceRoot, timeout));
  }

  /** 执行任务 */
  async executeTask(taskId: string): Promise<void> {
    const task = this.options.taskManager.getTask(taskId);
    if (!task) {
      throw new Error(`Task 不存在: ${taskId}`);
    }

    this.isRunning = true;
    this.options.taskManager.updateTaskStatus(taskId, 'running');

    try {
      const statuses = await this.cliDetector.checkAllCLIs();
      const availableCLIs = statuses.filter(s => s.available).map(s => s.type);

      if (availableCLIs.length === 0) {
        throw new Error('没有可用的 CLI 工具，请先安装至少一个 CLI (Claude/Codex/Gemini)');
      }

      const category = this.categorizeTask(task.prompt);
      const cli = this.selectBestCLI(category, availableCLIs);
      const files = this.extractTargetFiles(task.prompt);

      this.options.taskManager.addSubTask(taskId, task.prompt, cli, files);

      const updatedTask = this.options.taskManager.getTask(taskId);
      if (updatedTask) {
        await this.executeSubTasks(updatedTask);
      }

      this.options.taskManager.updateTaskStatus(taskId, 'completed');
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      const stack = error instanceof Error ? error.stack : undefined;

      console.error(`[Orchestrator] Task ${taskId} failed:`, msg);
      if (stack) {
        console.error('[Orchestrator] Stack trace:', stack);
      }

      globalEventBus.emitEvent('task:failed', {
        taskId,
        data: {
          error: msg,
          stack,
          timestamp: Date.now()
        }
      });

      this.options.taskManager.updateTaskStatus(taskId, 'failed');

      // 清理资源：中断所有正在运行的 Worker
      this.cleanupWorkers();

      throw error;
    } finally {
      this.isRunning = false;
    }
  }

  /** 清理 Worker 资源 */
  private cleanupWorkers(): void {
    try {
      for (const worker of this.workers.values()) {
        worker.interrupt();
      }
    } catch (error) {
      console.error('[Orchestrator] Failed to cleanup workers:', error);
    }
  }

  private categorizeTask(prompt: string): TaskCategory {
    const p = prompt.toLowerCase();
    if (['重构', '优化', 'refactor', 'optimize'].some(k => p.includes(k))) return 'refactor';
    if (['测试', 'test'].some(k => p.includes(k))) return 'test';
    if (['文档', '注释', 'doc', 'comment'].some(k => p.includes(k))) return 'document';
    if (['调试', 'debug', 'fix', 'bug'].some(k => p.includes(k))) return 'debug';
    if (['审查', 'review'].some(k => p.includes(k))) return 'review';
    if (['架构', 'architecture', 'design'].some(k => p.includes(k))) return 'architecture';
    if (['前端', 'frontend', 'ui', 'css'].some(k => p.includes(k))) return 'frontend';
    return 'implement';
  }

  private selectBestCLI(category: TaskCategory, available: CLIType[]): CLIType {
    const map: Record<TaskCategory, CLIType[]> = {
      'architecture': ['claude', 'gemini', 'codex'],
      'implement': ['claude', 'codex', 'gemini'],
      'backend': ['codex', 'claude', 'gemini'],
      'refactor': ['claude', 'codex', 'gemini'],
      'bugfix': ['claude', 'codex', 'gemini'],
      'debug': ['claude', 'codex', 'gemini'],
      'frontend': ['claude', 'gemini', 'codex'],
      'test': ['codex', 'claude', 'gemini'],
      'document': ['claude', 'gemini', 'codex'],
      'review': ['claude', 'gemini', 'codex'],
      'general': ['claude', 'codex', 'gemini'],
    };
    for (const cli of map[category] || []) {
      if (available.includes(cli)) return cli;
    }
    return available[0] || 'claude';
  }

  private extractTargetFiles(prompt: string): string[] {
    const m = prompt.match(/[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|html|json|md)/gi);
    return m ? [...new Set(m)] : [];
  }

  private async executeSubTasks(task: Task): Promise<WorkerResult[]> {
    const results: WorkerResult[] = [];
    const mode = this.options.mode || 'sequential';

    if (mode === 'parallel') {
      results.push(...await Promise.all(task.subTasks.map(st => this.executeSubTask(st))));
    } else {
      for (const st of task.subTasks) {
        const r = await this.executeSubTask(st);
        results.push(r);
        if (!r.success) break;
      }
    }
    return results;
  }

  private async executeSubTask(subTask: SubTask): Promise<WorkerResult> {
    const cli = subTask.assignedWorker || subTask.assignedCli;

    // 类型安全检查：确保 CLI 已分配
    if (!cli) {
      const error = `SubTask ${subTask.id} 没有分配 Worker`;
      console.error(`[Orchestrator] ${error}`);
      return {
        workerId: `unknown-${subTask.id}`,
        cliType: 'claude', // 默认值，避免类型错误
        success: false,
        error,
        duration: 0,
        timestamp: new Date()
      };
    }

    // 类型安全检查：确保 Worker 存在
    const worker = this.workers.get(cli);
    if (!worker) {
      const error = `Worker 不存在: ${cli}`;
      console.error(`[Orchestrator] ${error}`);
      return {
        workerId: `unknown-${subTask.id}`,
        cliType: cli,
        success: false,
        error,
        duration: 0,
        timestamp: new Date()
      };
    }

    // 创建文件快照（带错误处理）
    for (const f of subTask.targetFiles) {
      try {
        this.options.snapshotManager.createSnapshot(f, cli, subTask.id);
      } catch (error) {
        console.error(`[Orchestrator] Failed to create snapshot for ${f}:`, error);
        // 继续执行，快照失败不应阻止任务
      }
    }

    this.options.taskManager.updateSubTaskStatus(subTask.taskId, subTask.id, 'running');

    try {
      const result = await worker.execute({
        subTask,
        workingDirectory: this.options.workspaceRoot
      });

      this.options.taskManager.updateSubTaskStatus(
        subTask.taskId,
        subTask.id,
        result.success ? 'completed' : 'failed'
      );

      return result;
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      console.error(`[Orchestrator] SubTask ${subTask.id} execution failed:`, msg);

      this.options.taskManager.updateSubTaskStatus(subTask.taskId, subTask.id, 'failed');

      return {
        workerId: `${cli}-${subTask.id}`,
        cliType: cli,
        success: false,
        error: msg,
        duration: 0,
        timestamp: new Date()
      };
    }
  }

  interrupt(): void {
    if (!this.isRunning) return;
    for (const w of this.workers.values()) w.interrupt();
    this.isRunning = false;
  }

  get running(): boolean { return this.isRunning; }
  getWorker(cli: CLIType): BaseWorker | undefined { return this.workers.get(cli); }
}