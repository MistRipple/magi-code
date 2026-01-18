/**
 * InteractiveSession - 持续会话模式
 *
 * 用于主编排者，通过 --continue 参数保持会话上下文。
 * 每次消息启动新进程，但 Claude CLI 自动管理会话历史。
 * 天然保持完整对话历史，零额外 Token 开销（利用 Claude CLI 的缓存机制）。
 */

import { EventEmitter } from 'events';
import { spawn, ChildProcessWithoutNullStreams } from 'child_process';
import * as crypto from 'crypto';
import type { SessionMessage, SessionProcess, SessionResponse } from './types';
import type { CLIType } from '../types';
import type { CLIQuestion } from './print-session';

export interface InteractiveSessionOptions {
  cli: CLIType;
  cwd: string;
  env?: Record<string, string>;
  command: string;
  args?: string[];
  idleTimeoutMs?: number;
  /** 会话 ID，用于恢复（可选，Claude CLI 会自动管理） */
  sessionId?: string;
}

/**
 * 询问检测结果
 */
export interface QuestionDetectionResult {
  isQuestion: boolean;
  type: 'structured' | 'permission' | 'confirmation' | 'input' | 'unknown';
  data?: Record<string, unknown>;
  originalText?: string;
}

export class InteractiveSession extends EventEmitter implements SessionProcess {
  readonly cli: CLIType;
  private readonly cwd: string;
  private readonly env: Record<string, string>;
  private readonly command: string;
  private readonly baseArgs: string[];
  private readonly idleTimeoutMs: number;
  private readonly sessionId?: string;

  private currentProcess?: ChildProcessWithoutNullStreams;
  private alive = false;
  private waitingForAnswer = false;
  private isFirstMessage = true;
  /** 当前询问 ID */
  private currentQuestionId?: string;
  /** 上一次询问 ID（用于去重） */
  private lastQuestionId?: string;

  constructor(options: InteractiveSessionOptions) {
    super();
    this.cli = options.cli;
    this.cwd = options.cwd;
    this.command = options.command;
    this.baseArgs = options.args ?? [];
    this.idleTimeoutMs = options.idleTimeoutMs ?? 300000; // 5分钟
    this.sessionId = options.sessionId;

    // 合并环境变量
    const mergedEnv: Record<string, string> = {};
    const rawEnv = { ...process.env, ...options.env };
    Object.entries(rawEnv).forEach(([key, value]) => {
      if (value !== undefined) {
        mergedEnv[key] = value;
      }
    });
    this.env = mergedEnv;
  }

  get isAlive(): boolean {
    return this.alive;
  }

  get isWaitingForAnswer(): boolean {
    return this.waitingForAnswer;
  }

  /**
   * 启动会话（标记为就绪状态）
   */
  async start(): Promise<void> {
    this.alive = true;
    this.emit('log', `[InteractiveSession] ${this.cli} 会话已就绪 (cwd: ${this.cwd})`);
  }

  /**
   * 停止会话
   */
  async stop(): Promise<void> {
    if (this.currentProcess && !this.currentProcess.killed) {
      this.currentProcess.kill('SIGTERM');
      this.currentProcess = undefined;
    }
    this.alive = false;
    this.emit('log', `[InteractiveSession] ${this.cli} 会话已停止`);
  }

  /**
   * 发送消息
   * 每次消息启动新进程，使用 --continue 保持会话上下文
   */
  async send(message: SessionMessage): Promise<SessionResponse> {
    if (!this.alive) {
      throw new Error(`${this.cli} 会话未启动`);
    }

    return new Promise<SessionResponse>((resolve, reject) => {
      // 构建参数：首次消息可能需要 --session-id，后续使用 --continue
      const args = this.buildArgs(message);

      this.emit('log', `[InteractiveSession] 启动进程: ${this.command} ${args.slice(0, 5).join(' ')}...`);

      const proc = spawn(this.command, [...args, message.content], {
        cwd: this.cwd,
        env: this.env,
      });

      this.currentProcess = proc;

      let stdout = '';
      let stderr = '';
      let jsonBuffer = '';

      const timeoutId = setTimeout(() => {
        proc.kill('SIGTERM');
        reject(new Error(`${this.cli} 响应超时 (${this.idleTimeoutMs}ms)`));
      }, this.idleTimeoutMs);

      proc.stdout.on('data', (data) => {
        const text = data.toString();
        stdout += text;
        jsonBuffer += text;
        this.emit('output', text);

        // 🔧 修复：检测 CLI 询问并发送正确格式的事件
        if (this.detectQuestion(text)) {
          // 如果已经在等待回答，不重复触发
          if (this.waitingForAnswer) {
            return;
          }

          // 基于内容生成稳定的 questionId
          const contentHash = crypto.createHash('md5')
            .update(text.trim())
            .digest('hex')
            .slice(0, 8);

          this.currentQuestionId = `${message.requestId || 'interactive'}-${contentHash}`;

          // 检查是否已经发送过相同的询问
          if (this.lastQuestionId === this.currentQuestionId) {
            return;
          }

          this.lastQuestionId = this.currentQuestionId;
          this.waitingForAnswer = true;

          // 🔧 修复：发送正确格式的 CLIQuestion 对象
          const question: CLIQuestion = {
            questionId: this.currentQuestionId,
            cli: this.cli,
            content: text.trim(),
            pattern: 'interactive-detection',
            timestamp: Date.now(),
          };

          this.emit('question', question);
        }
      });

      proc.stderr.on('data', (data) => {
        const text = data.toString();
        stderr += text;
        this.emit('stderr', text);
      });

      proc.on('error', (error) => {
        clearTimeout(timeoutId);
        reject(error);
      });

      proc.on('close', (code) => {
        clearTimeout(timeoutId);
        this.currentProcess = undefined;
        this.waitingForAnswer = false;
        this.isFirstMessage = false;

        this.emit('log', `[InteractiveSession] ${this.cli} 进程退出 (code: ${code})`);

        if (code !== 0 && code !== null) {
          // 检查是否是正常的中断
          if (code === 143 || code === 130) {
            reject(new Error(`${this.cli} 进程被中断`));
            return;
          }
          reject(new Error(`${this.cli} 进程异常退出 (code: ${code}): ${stderr}`));
          return;
        }

        // 解析 stream-json 响应
        const response = this.parseStreamJsonResponse(message.requestId, stdout, jsonBuffer);
        resolve(response);
      });

      // 关闭 stdin（消息通过参数传递）
      proc.stdin.end();
    });
  }

  /**
   * 构建命令行参数
   */
  private buildArgs(message: SessionMessage): string[] {
    const args = [...this.baseArgs];

    // 确保有 -p 参数（print 模式）
    if (!args.includes('-p') && !args.includes('--print')) {
      args.unshift('-p');
    }

    // 添加 --continue 参数保持会话上下文
    if (!args.includes('--continue')) {
      args.push('--continue');
    }

    return args;
  }

  /**
   * 增强的 CLI 询问检测
   * 支持结构化询问、权限请求、确认请求等多种类型
   */
  private detectQuestionEnhanced(text: string): QuestionDetectionResult {
    // 1. 检查 stream-json 中的结构化询问标记
    const structuredMatch = text.match(/\{"type"\s*:\s*"(question|input_request|permission)"[^}]*\}/);
    if (structuredMatch) {
      try {
        const data = JSON.parse(structuredMatch[0]);
        return {
          isQuestion: true,
          type: 'structured',
          data,
          originalText: text,
        };
      } catch {
        // 解析失败，继续其他检测
      }
    }

    // 2. 检查 Claude CLI 特有的权限请求格式
    // 例如: "Allow Claude to edit file.ts? [Y/n]"
    const permissionPatterns = [
      /Allow\s+\w+\s+to\s+.+\?\s*\[Y\/n\]/i,
      /Do you want to allow\s+.+\?\s*\[Y\/n\]/i,
      /Permission\s+required.+\[Y\/n\]/i,
      /Approve\s+.+\?\s*\[Y\/n\]/i,
    ];
    for (const pattern of permissionPatterns) {
      if (pattern.test(text)) {
        return {
          isQuestion: true,
          type: 'permission',
          originalText: text,
        };
      }
    }

    // 3. 检查确认请求
    const confirmationPatterns = [
      /\[y\/n\]/i,
      /\(yes\/no\)/i,
      /\[Y\/n\]/,
      /\[y\/N\]/,
      /confirm\?/i,
      /proceed\?/i,
      /continue\?/i,
      /are you sure\?/i,
    ];
    for (const pattern of confirmationPatterns) {
      if (pattern.test(text)) {
        return {
          isQuestion: true,
          type: 'confirmation',
          originalText: text,
        };
      }
    }

    // 4. 检查输入请求
    const inputPatterns = [
      /enter\s+.+:/i,
      /input\s+.+:/i,
      /provide\s+.+:/i,
      /type\s+.+:/i,
      /press enter/i,
      /waiting for input/i,
    ];
    for (const pattern of inputPatterns) {
      if (pattern.test(text)) {
        return {
          isQuestion: true,
          type: 'input',
          originalText: text,
        };
      }
    }

    // 5. 通用问号检测（最后的回退）
    // 只有当文本以问号结尾且不是代码注释时才认为是问题
    const trimmedText = text.trim();
    if (trimmedText.endsWith('?') && !trimmedText.startsWith('//') && !trimmedText.startsWith('#')) {
      // 排除代码中的三元运算符等
      const lastLine = trimmedText.split('\n').pop() || '';
      if (lastLine.endsWith('?') && !lastLine.includes('?') || lastLine.match(/\?\s*$/)) {
        return {
          isQuestion: true,
          type: 'unknown',
          originalText: text,
        };
      }
    }

    return {
      isQuestion: false,
      type: 'unknown',
    };
  }

  /**
   * 检测 CLI 询问（保持向后兼容）
   */
  private detectQuestion(text: string): boolean {
    const result = this.detectQuestionEnhanced(text);
    return result.isQuestion;
  }

  /**
   * 获取询问类型（供外部使用）
   */
  getQuestionType(text: string): string {
    const result = this.detectQuestionEnhanced(text);
    return result.type;
  }

  /**
   * 解析 stream-json 响应
   */
  private parseStreamJsonResponse(requestId: string, stdout: string, jsonBuffer: string): SessionResponse {
    const lines = jsonBuffer.split('\n');
    let resultJson: Record<string, unknown> = {};
    const contents: string[] = [];

    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed) continue;

      try {
        const json = JSON.parse(trimmed);

        // 提取 assistant 消息内容
        if (json.type === 'assistant' && json.message?.content) {
          for (const block of json.message.content) {
            if (block.type === 'text') {
              contents.push(block.text);
            }
          }
        }

        // 保存 result 信息
        if (json.type === 'result') {
          resultJson = json;
        }
      } catch {
        // 忽略非 JSON 行
      }
    }

    const response: SessionResponse = {
      requestId,
      content: contents.join('\n') || (resultJson.result as string) || '',
      raw: stdout,
      metadata: resultJson,
    };

    // 提取 token 使用信息
    if (resultJson.usage) {
      const usage = resultJson.usage as Record<string, number>;
      response.tokenUsage = {
        inputTokens: usage.input_tokens || 0,
        outputTokens: usage.output_tokens || 0,
        cacheReadTokens: usage.cache_read_input_tokens,
      };
    }

    return response;
  }

  /**
   * 向 CLI 发送用户输入（用于回答询问）
   */
  writeInput(text: string): boolean {
    if (!this.currentProcess || this.currentProcess.killed) {
      return false;
    }

    try {
      const input = text.endsWith('\n') ? text : text + '\n';
      this.currentProcess.stdin.write(input);
      this.waitingForAnswer = false;
      return true;
    } catch {
      return false;
    }
  }

  /**
   * 中断当前操作
   */
  async interrupt(reason?: string): Promise<void> {
    if (this.currentProcess && !this.currentProcess.killed) {
      this.currentProcess.kill('SIGINT');
    }
    if (reason) {
      this.emit('log', `[InteractiveSession] 中断: ${reason}`);
    }
  }

  /**
   * 获取会话 ID
   */
  getSessionId(): string | undefined {
    return this.sessionId;
  }
}
