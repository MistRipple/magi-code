/**
 * Gemini Worker
 * Gemini CLI 执行器
 */

import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { BaseWorker } from './base-worker';

/**
 * Gemini Worker 配置
 */
export interface GeminiWorkerConfig extends Omit<WorkerConfig, 'sandbox'> {
  model?: string;
  sandbox?: boolean;
}

/**
 * Gemini CLI Worker
 */
export class GeminiWorker extends BaseWorker {
  private geminiConfig: GeminiWorkerConfig;

  constructor(config: GeminiWorkerConfig) {
    // 转换为 WorkerConfig，排除 sandbox 布尔值
    const { sandbox: _sandbox, ...baseConfig } = config;
    super({ ...baseConfig, cliType: 'gemini' });
    this.geminiConfig = config;
  }

  get cliType(): CLIType {
    return 'gemini';
  }

  /** 构建 Gemini CLI 命令参数 */
  protected buildArgs(subTask: SubTask): string[] {
    const args: string[] = [];

    // 添加提示内容
    args.push('-p', this.buildPrompt(subTask));

    // 非交互模式
    args.push('--non-interactive');

    // 自动运行工具
    args.push('--auto-run-tools');

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

  /** 解析 Gemini CLI 输出 */
  protected parseOutput(output: string): Partial<WorkerResult> {
    // Gemini 输出解析
    return {};
  }
}

/** 创建 Gemini Worker 的工厂函数 */
export function createGeminiWorker(
  cliPath: string = 'gemini',
  workingDirectory: string,
  timeout: number = 300000
): GeminiWorker {
  return new GeminiWorker({
    cliType: 'gemini',
    cliPath,
    timeout,
    workingDirectory,
  });
}

