/**
 * Codex Worker
 * Codex CLI 执行器
 */

import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { BaseWorker } from './base-worker';

/**
 * Codex Worker 配置
 */
export interface CodexWorkerConfig extends WorkerConfig {
  model?: string;
  approval?: 'suggest' | 'auto-edit' | 'full-auto';
}

/**
 * Codex CLI Worker
 */
export class CodexWorker extends BaseWorker {
  private codexConfig: CodexWorkerConfig;

  constructor(config: CodexWorkerConfig) {
    super(config);
    this.codexConfig = config;
  }

  get cliType(): CLIType {
    return 'codex';
  }

  /** 构建 Codex CLI 命令参数 */
  protected buildArgs(subTask: SubTask): string[] {
    const args: string[] = [];

    // 使用 exec 模式直接执行
    args.push('exec');

    // 添加提示内容
    args.push(this.buildPrompt(subTask));

    // 自动批准模式
    const approval = this.codexConfig.approval ?? 'full-auto';
    args.push('--approval', approval);

    // 静默模式
    args.push('--quiet');

    return args;
  }

  /** 构建提示词 */
  private buildPrompt(subTask: SubTask): string {
    let prompt = subTask.description;

    if (subTask.targetFiles.length > 0) {
      prompt += `\n\n目标文件: ${subTask.targetFiles.join(', ')}`;
    }

    prompt += '\n\n请直接修改文件，完成后简要说明所做的更改。';

    return prompt;
  }

  /** 解析 Codex CLI 输出 */
  protected parseOutput(output: string): Partial<WorkerResult> {
    // Codex 输出解析
    return {};
  }
}

/** 创建 Codex Worker 的工厂函数 */
export function createCodexWorker(
  cliPath: string = 'codex',
  workingDirectory: string,
  timeout: number = 300000
): CodexWorker {
  return new CodexWorker({
    cliType: 'codex',
    cliPath,
    timeout,
    workingDirectory,
    approval: 'full-auto',
  });
}

