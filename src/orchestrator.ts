/**
 * Orchestrator - 核心编排器
 * 负责任务分解、Worker 调度、结果收集
 */

import { CLIType, Task, SubTask, TaskCategory, WorkerResult, ExecutionMode } from './types';
import { SessionManager } from './session-manager';
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
  sessionManager: SessionManager;
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
    if (!task) throw new Error(`Task 不存在: ${taskId}`);

    this.isRunning = true;
    this.options.taskManager.updateTaskStatus(taskId, 'running');

    try {
      const statuses = await this.cliDetector.checkAllCLIs();
      const availableCLIs = statuses.filter(s => s.available).map(s => s.type);
      if (availableCLIs.length === 0) throw new Error('没有可用的 CLI 工具');

      const category = this.categorizeTask(task.prompt);
      const cli = this.selectBestCLI(category, availableCLIs);
      const files = this.extractTargetFiles(task.prompt);

      this.options.taskManager.addSubTask(taskId, task.prompt, cli, files);

      const updatedTask = this.options.taskManager.getTask(taskId);
      if (updatedTask) await this.executeSubTasks(updatedTask);

      this.options.taskManager.updateTaskStatus(taskId, 'completed');
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      globalEventBus.emitEvent('task:failed', { taskId, data: { error: msg } });
      this.options.taskManager.updateTaskStatus(taskId, 'failed');
      throw error;
    } finally {
      this.isRunning = false;
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
    const worker = this.workers.get(cli!);
    if (!worker) {
      return { workerId: `unknown-${subTask.id}`, cliType: cli!, success: false,
        error: `Worker 不存在: ${cli}`, duration: 0, timestamp: new Date() };
    }

    for (const f of subTask.targetFiles) {
      this.options.snapshotManager.createSnapshot(f, cli!, subTask.id);
    }

    this.options.taskManager.updateSubTaskStatus(subTask.taskId, subTask.id, 'running');
    const result = await worker.execute({ subTask, workingDirectory: this.options.workspaceRoot });
    this.options.taskManager.updateSubTaskStatus(subTask.taskId, subTask.id, result.success ? 'completed' : 'failed');
    return result;
  }

  interrupt(): void {
    if (!this.isRunning) return;
    for (const w of this.workers.values()) w.interrupt();
    this.isRunning = false;
  }

  get running(): boolean { return this.isRunning; }
  getWorker(cli: CLIType): BaseWorker | undefined { return this.workers.get(cli); }
}