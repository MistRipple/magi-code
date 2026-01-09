/**
 * Claude Worker
 * Claude CLI 执行器
 */

import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { BaseWorker } from './base-worker';

/**
 * Claude Worker 配置
 */
export interface ClaudeWorkerConfig extends WorkerConfig {
  model?: string;
  maxTokens?: number;
}

/**
 * Claude CLI Worker
 */
export class ClaudeWorker extends BaseWorker {
  private claudeConfig: ClaudeWorkerConfig;

  constructor(config: ClaudeWorkerConfig) {
    super(config);
    this.claudeConfig = config;
  }

  get cliType(): CLIType {
    return 'claude';
  }

  /** 构建 Claude CLI 命令参数 */
  protected buildArgs(subTask: SubTask): string[] {
    const args: string[] = [];

    // 添加提示内容
    args.push('-p', this.buildPrompt(subTask));

    // 允许编辑的文件
    if (subTask.targetFiles.length > 0) {
      for (const file of subTask.targetFiles) {
        args.push('--allowedTools', `Edit:${file}`);
      }
    }

    // 非交互模式
    args.push('--no-input');

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

  /** 解析 Claude CLI 输出 */
  protected parseOutput(output: string): Partial<WorkerResult> {
    // 提取修改的文件列表
    const modifiedFiles: string[] = [];
    
    // 匹配 "Edited file: xxx" 或 "Modified: xxx" 等模式
    const editPatterns = [
      /Edited?\s+(?:file:?\s*)?([^\n]+)/gi,
      /Modified:?\s*([^\n]+)/gi,
      /Created:?\s*([^\n]+)/gi,
      /Updated:?\s*([^\n]+)/gi,
    ];

    for (const pattern of editPatterns) {
      let match;
      while ((match = pattern.exec(output)) !== null) {
        const file = match[1].trim();
        if (file && !modifiedFiles.includes(file)) {
          modifiedFiles.push(file);
        }
      }
    }

    return {
      // 可以在这里添加更多解析逻辑
    };
  }
}

/** 创建 Claude Worker 的工厂函数 */
export function createClaudeWorker(
  cliPath: string = 'claude',
  workingDirectory: string,
  timeout: number = 300000
): ClaudeWorker {
  return new ClaudeWorker({
    cliType: 'claude',
    cliPath,
    timeout,
    workingDirectory,
  });
}

