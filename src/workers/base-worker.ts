/**
 * Worker 基类
 * 定义 CLI 执行的抽象接口
 */

import { ChildProcess, spawn } from 'child_process';
import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { EventEmitter, globalEventBus } from '../events';

/** Worker 执行选项 */
export interface WorkerExecuteOptions {
  subTask: SubTask;
  workingDirectory: string;
  timeout?: number;
  onOutput?: (output: string) => void;
}

/**
 * 抽象 Worker 基类
 * 各 CLI Worker 需要继承此类并实现抽象方法
 */
export abstract class BaseWorker extends EventEmitter {
  protected config: WorkerConfig;
  protected process: ChildProcess | null = null;
  protected isRunning = false;
  protected outputBuffer: string[] = [];

  constructor(config: WorkerConfig) {
    super();
    this.config = config;
  }

  /** CLI 类型 */
  abstract get cliType(): CLIType;

  /** 构建 CLI 命令参数 */
  protected abstract buildArgs(subTask: SubTask): string[];

  /** 解析 CLI 输出 */
  protected abstract parseOutput(output: string): Partial<WorkerResult>;

  /** 执行子任务 */
  async execute(options: WorkerExecuteOptions): Promise<WorkerResult> {
    const { subTask, workingDirectory, timeout, onOutput } = options;
    const startTime = Date.now();
    this.isRunning = true;
    this.outputBuffer = [];

    globalEventBus.emitEvent('subtask:started', {
      taskId: subTask.taskId, subTaskId: subTask.id,
    });

    try {
      const args = this.buildArgs(subTask);
      const output = await this.runProcess(args, workingDirectory, timeout, onOutput);
      const parsed = this.parseOutput(output);

      const result: WorkerResult = {
        workerId: `${this.cliType}-${subTask.id}`,
        cliType: this.cliType,
        success: true,
        output,
        duration: Date.now() - startTime,
        timestamp: new Date(),
        ...parsed,
      };

      globalEventBus.emitEvent('subtask:completed', {
        taskId: subTask.taskId, subTaskId: subTask.id, data: result,
      });
      return result;
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      globalEventBus.emitEvent('subtask:failed', {
        taskId: subTask.taskId,
        subTaskId: subTask.id,
        data: { error: errorMessage, cli: this.cliType },
      });
      return {
        workerId: `${this.cliType}-${subTask.id}`,
        cliType: this.cliType,
        success: false,
        error: errorMessage,
        duration: Date.now() - startTime,
        timestamp: new Date(),
      };
    } finally {
      this.isRunning = false;
      this.process = null;
    }
  }

  /** 运行 CLI 进程 */
  protected runProcess(
    args: string[], cwd: string, timeout?: number, onOutput?: (output: string) => void
  ): Promise<string> {
    return new Promise((resolve, reject) => {
      const effectiveTimeout = timeout ?? this.config.timeout;
      let output = '';
      let timeoutId: NodeJS.Timeout | undefined;

      this.process = spawn(this.config.cliPath, args, { cwd, shell: true, env: { ...process.env } });

      if (effectiveTimeout > 0) {
        timeoutId = setTimeout(() => {
          this.interrupt();
          reject(new Error(`执行超时 (${effectiveTimeout}ms)`));
        }, effectiveTimeout);
      }

      this.process.stdout?.on('data', (data: Buffer) => {
        const chunk = data.toString();
        output += chunk;
        this.outputBuffer.push(chunk);
        onOutput?.(chunk);
        globalEventBus.emitEvent('subtask:output', { data: { output: chunk, cliType: this.cliType } });
      });

      this.process.stderr?.on('data', (data: Buffer) => {
        const chunk = data.toString();
        output += chunk;
        this.outputBuffer.push(chunk);
        onOutput?.(chunk);
      });

      this.process.on('close', (code) => {
        if (timeoutId) clearTimeout(timeoutId);
        code === 0 ? resolve(output) : reject(new Error(`进程退出码: ${code}\n${output}`));
      });

      this.process.on('error', (error) => {
        if (timeoutId) clearTimeout(timeoutId);
        reject(error);
      });
    });
  }

  /** 打断执行 */
  interrupt(): boolean {
    if (this.process && this.isRunning) {
      this.process.kill('SIGTERM');
      this.isRunning = false;
      return true;
    }
    return false;
  }

  get running(): boolean { return this.isRunning; }
  getOutput(): string[] { return [...this.outputBuffer]; }
  getConfig(): WorkerConfig { return { ...this.config }; }
}
