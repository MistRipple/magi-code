/**
 * Shell 执行器
 * 提供安全的 Shell 命令执行能力
 */

import { exec } from 'child_process';
import { promisify } from 'util';
import { ShellExecuteOptions, ShellExecuteResult } from './types';
import { logger, LogCategory } from '../logging';

const execAsync = promisify(exec);

/**
 * Shell 执行器
 */
export class ShellExecutor {
  private readonly defaultTimeout: number = 30000; // 30 秒
  private readonly maxTimeout: number = 300000; // 5 分钟

  /**
   * 执行 Shell 命令
   */
  async execute(options: ShellExecuteOptions): Promise<ShellExecuteResult> {
    const startTime = Date.now();
    const timeout = Math.min(
      options.timeout || this.defaultTimeout,
      this.maxTimeout
    );

    logger.debug('Executing shell command', {
      command: options.command,
      cwd: options.cwd,
      timeout,
    }, LogCategory.SHELL);

    try {
      const { stdout, stderr } = await execAsync(options.command, {
        cwd: options.cwd,
        timeout,
        env: { ...process.env, ...options.env },
        maxBuffer: 10 * 1024 * 1024, // 10MB
      });

      const duration = Date.now() - startTime;

      const result: ShellExecuteResult = {
        stdout: stdout.trim(),
        stderr: stderr.trim(),
        exitCode: 0,
        duration,
      };

      logger.debug('Shell command completed', {
        command: options.command,
        exitCode: result.exitCode,
        duration,
        stdoutLength: result.stdout.length,
        stderrLength: result.stderr.length,
      }, LogCategory.SHELL);

      return result;
    } catch (error: any) {
      const duration = Date.now() - startTime;

      const result: ShellExecuteResult = {
        stdout: error.stdout?.trim() || '',
        stderr: error.stderr?.trim() || error.message,
        exitCode: error.code || 1,
        duration,
      };

      logger.error('Shell command failed', {
        command: options.command,
        exitCode: result.exitCode,
        duration,
        error: error.message,
      }, LogCategory.SHELL);

      return result;
    }
  }

  /**
   * 验证命令是否安全
   */
  validateCommand(command: string): { valid: boolean; reason?: string } {
    // 基本的安全检查
    const dangerousPatterns = [
      /rm\s+-rf\s+\//, // 删除根目录
      /:\(\)\{.*\}/, // Fork bomb
      />\s*\/dev\/sda/, // 写入磁盘设备
    ];

    for (const pattern of dangerousPatterns) {
      if (pattern.test(command)) {
        return {
          valid: false,
          reason: `Command contains dangerous pattern: ${pattern}`,
        };
      }
    }

    return { valid: true };
  }

  /**
   * 获取工具定义（用于 LLM）
   */
  getToolDefinition() {
    return {
      name: 'execute_shell',
      description: 'Execute a shell command and return the output. Use this for running terminal commands, scripts, or system operations.',
      input_schema: {
        type: 'object' as const,
        properties: {
          command: {
            type: 'string' as const,
            description: 'The shell command to execute',
            required: true,
          },
          cwd: {
            type: 'string' as const,
            description: 'Working directory for the command (optional)',
            required: false,
          },
          timeout: {
            type: 'number' as const,
            description: 'Timeout in milliseconds (default: 30000, max: 300000)',
            required: false,
          },
        },
        required: ['command'],
      },
    };
  }
}
