import { logger, LogCategory } from '../../logging';
import { EventEmitter } from 'events';
import { spawn, ChildProcessWithoutNullStreams } from 'child_process';
import * as crypto from 'crypto';
import type { SessionMessage, SessionProcess, SessionProcessOptions, SessionResponse } from './types';
import type { CLIType } from '../types';

export interface PrintSessionOptions extends SessionProcessOptions {
  cli: CLIType;
  command: string;
  args?: string[];
}

/**
 * CLI 询问信息
 */
export interface CLIQuestion {
  /** 询问 ID，用于匹配回答 */
  questionId: string;
  /** CLI 类型 */
  cli: CLIType;
  /** 询问内容（CLI 输出的原始文本） */
  content: string;
  /** 检测到的询问模式 */
  pattern: string;
  /** 询问时间 */
  timestamp: number;
}

/**
 * 常见的 CLI 询问模式
 */
const QUESTION_PATTERNS = [
  // Claude CLI 询问模式
  /Answer questions\?\s*\(y\/n\)/i,
  /Do you want to continue\?\s*\[?y\/n\]?/i,
  /Continue\?\s*\[?y\/n\]?/i,
  /Proceed\?\s*\[?y\/n\]?/i,
  // 通用确认模式
  /\(y\/n\)\s*:?\s*$/i,
  /\[y\/N\]\s*:?\s*$/i,
  /\[Y\/n\]\s*:?\s*$/i,
  // 输入提示模式
  /Press Enter to continue/i,
  /Type ['"]?yes['"]? to confirm/i,
  /Enter your choice/i,
  // 以问号结尾且等待输入的行
  /\?\s*$/,
];

export class PrintSession extends EventEmitter implements SessionProcess {
  readonly cli: CLIType;
  private readonly cwd: string;
  private readonly env: Record<string, string>;
  private readonly idleTimeoutMs: number;
  private readonly command: string;
  private readonly args: string[];
  private alive = false;
  private current?: ChildProcessWithoutNullStreams;

  /** 当前是否在等待用户回答 */
  private waitingForAnswer = false;
  /** 当前询问 ID */
  private currentQuestionId?: string;
  /** 上一次询问 ID（用于去重） */
  private lastQuestionId?: string;
  /** 询问超时定时器 */
  private questionTimeoutId?: NodeJS.Timeout;
  /** 询问超时时间（毫秒） */
  private readonly questionTimeoutMs = 60000; // 60秒

  constructor(options: PrintSessionOptions) {
    super();
    this.cli = options.cli;
    this.cwd = options.cwd;
    const mergedEnv: Record<string, string> = {};
    const rawEnv = { ...process.env, ...options.env };
    Object.entries(rawEnv).forEach(([key, value]) => {
      if (value !== undefined) {
        mergedEnv[key] = value;
      }
    });
    this.env = mergedEnv;
    this.idleTimeoutMs = options.idleTimeoutMs ?? 120000;
    this.command = options.command;
    this.args = options.args ?? [];
  }

  get isAlive(): boolean {
    return this.alive;
  }

  /** 是否正在等待用户回答 */
  get isWaitingForAnswer(): boolean {
    return this.waitingForAnswer;
  }

  async start(): Promise<void> {
    this.alive = true;
  }

  async stop(): Promise<void> {
    this.clearQuestionTimeout();
    if (this.current) {
      this.current.kill();
      this.current = undefined;
    }
    this.alive = false;
    this.waitingForAnswer = false;
  }

  async interrupt(reason?: string): Promise<void> {
    this.clearQuestionTimeout();
    if (this.current) {
      this.current.kill('SIGINT');
    }
    this.waitingForAnswer = false;
    if (reason) {
      this.emit('log', `[${this.cli}] interrupt: ${reason}`);
    }
  }

  /**
   * 向 CLI 发送用户回答
   */
  writeInput(text: string): boolean {
    if (!this.current || !this.current.stdin || !this.waitingForAnswer) {
      logger.info('CLI.打印_会话.写入_输入.失败', { reason: 'not_waiting_or_no_process' }, LogCategory.CLI);
      return false;
    }

    this.clearQuestionTimeout();
    this.waitingForAnswer = false;
    this.currentQuestionId = undefined;

    try {
      // 发送用户输入，添加换行符
      const input = text.endsWith('\n') ? text : text + '\n';
      this.current.stdin.write(input);
      logger.info('CLI.打印_会话.写入_输入.成功', { length: text.length }, LogCategory.CLI);
      return true;
    } catch (error) {
      logger.error('CLI.打印_会话.写入_输入.错误', error, LogCategory.CLI);
      return false;
    }
  }

  async send(message: SessionMessage): Promise<SessionResponse> {
    if (!this.alive) {
      throw new Error(`${this.cli} session is not running`);
    }

    const args = [...this.args, message.content];
    return new Promise<SessionResponse>((resolve, reject) => {
      const child = spawn(this.command, args, {
        cwd: this.cwd,
        env: this.env,
      });
      this.current = child;

      // 不再立即关闭 stdin，保持打开以支持交互式询问
      // if (child.stdin) {
      //   child.stdin.end();
      // }

      let stdout = '';
      let stderr = '';
      let outputBuffer = ''; // 用于检测询问的缓冲区

      const timeoutId = setTimeout(() => {
        this.clearQuestionTimeout();
        child.kill('SIGTERM');
        reject(new Error(`${this.cli} session timeout`));
      }, this.idleTimeoutMs);

      child.stdout.on('data', (data) => {
        const text = data.toString();
        stdout += text;
        outputBuffer += text;
        this.emit('output', text);

        // 检测是否有询问
        this.checkForQuestion(outputBuffer, message.requestId);
      });

      child.stderr.on('data', (data) => {
        const text = data.toString();
        stderr += text;
        outputBuffer += text;
        this.emit('output', text);

        // stderr 也可能包含询问
        this.checkForQuestion(outputBuffer, message.requestId);
      });

      child.on('error', (error) => {
        clearTimeout(timeoutId);
        this.clearQuestionTimeout();
        reject(error instanceof Error ? error : new Error(String(error)));
      });

      child.on('close', (code) => {
        clearTimeout(timeoutId);
        this.clearQuestionTimeout();
        this.current = undefined;
        this.waitingForAnswer = false;

        if (code && code !== 0) {
          reject(new Error(stderr.trim() || `${this.cli} session exited with code ${code}`));
          return;
        }
        const parsed = this.parseResponse(stdout);
        const content = parsed
          ? (parsed.content ?? '')
          : stdout.trim();
        const tokenUsage = parsed?.tokenUsage ?? {
          inputTokens: this.estimateTokens(message.content),
          outputTokens: this.estimateTokens(content),
        };
        resolve({
          requestId: message.requestId,
          content,
          raw: `${stdout}${stderr}`,
          metadata: message.metadata,
          tokenUsage,
        });
      });
    });
  }

  /**
   * 检测输出中是否包含询问
   */
  private checkForQuestion(buffer: string, requestId: string): void {
    // 如果已经在等待回答，不重复检测
    if (this.waitingForAnswer) {
      return;
    }

    // 获取最后几行进行检测
    const lines = buffer.split('\n');
    const lastLines = lines.slice(-5).join('\n').trim();

    // 🔧 修复：如果内容为空，不触发询问
    if (!lastLines) {
      return;
    }

    for (const pattern of QUESTION_PATTERNS) {
      if (pattern.test(lastLines)) {
        // 🔧 修复：基于内容生成稳定的 questionId
        const contentHash = crypto.createHash('md5')
          .update(lastLines)
          .digest('hex')
          .slice(0, 8);

        this.currentQuestionId = `${requestId}-${contentHash}`;

        // 🔧 修复：检查是否已经发送过相同的询问
        if (this.lastQuestionId === this.currentQuestionId) {
          logger.debug('CLI.打印_会话.提问.重复', { questionId: this.currentQuestionId }, LogCategory.CLI);
          return;
        }

        this.lastQuestionId = this.currentQuestionId;
        this.waitingForAnswer = true;

        const question: CLIQuestion = {
          questionId: this.currentQuestionId,
          cli: this.cli,
          content: lastLines,
          pattern: pattern.source,
          timestamp: Date.now(),
        };

        logger.info('CLI.打印_会话.提问.检测到', { question }, LogCategory.CLI);
        this.emit('question', question);

        // 设置询问超时
        this.setQuestionTimeout();
        break;
      }
    }
  }

  /**
   * 设置询问超时
   */
  private setQuestionTimeout(): void {
    this.clearQuestionTimeout();
    this.questionTimeoutId = setTimeout(() => {
      if (this.waitingForAnswer) {
        logger.info('CLI.打印_会话.提问.超时', { questionId: this.currentQuestionId, cli: this.cli }, LogCategory.CLI);
        // 超时后发送默认回答（通常是 'n' 或空行）
        this.writeInput('n');
        this.emit('questionTimeout', {
          questionId: this.currentQuestionId,
          cli: this.cli,
        });
      }
    }, this.questionTimeoutMs);
  }

  /**
   * 清除询问超时
   */
  private clearQuestionTimeout(): void {
    if (this.questionTimeoutId) {
      clearTimeout(this.questionTimeoutId);
      this.questionTimeoutId = undefined;
    }
  }

  private estimateTokens(text: string): number {
    const trimmed = text.trim();
    if (!trimmed) return 0;
    return Math.max(1, Math.ceil(trimmed.length / 4));
  }

  private parseResponse(raw: string): { content?: string; tokenUsage?: SessionResponse['tokenUsage'] } | null {
    const trimmed = raw.trim();
    if (!trimmed) return null;
    const parsed = this.tryParseJson(trimmed);
    if (!parsed) {
      const streamContent = this.extractContentFromStream(raw);
      if (!streamContent) return null;
      return { content: streamContent };
    }
    let content = this.extractContent(parsed);
    if (!content) {
      content = this.extractContentFromStream(raw);
    }
    const tokenUsage = this.extractTokenUsage(parsed);
    return { content, tokenUsage };
  }

  private extractContentFromStream(raw: string): string | undefined {
    const lines = raw.split('\n').map(line => line.trim()).filter(Boolean);
    let lastContent: string | undefined;
    for (const line of lines) {
      if (!line.startsWith('{') && !line.startsWith('[')) continue;
      const parsed = this.tryParseJson(line);
      if (!parsed) continue;
      const extracted = this.extractContent(parsed);
      if (extracted && extracted.trim()) {
        lastContent = extracted;
      }
    }
    return lastContent;
  }

  private tryParseJson(raw: string): any | null {
    const trimmed = raw.trim();
    if (!trimmed) return null;
    if ((trimmed.startsWith('{') && trimmed.endsWith('}')) || (trimmed.startsWith('[') && trimmed.endsWith(']'))) {
      try {
        return JSON.parse(trimmed);
      } catch {
        // fallthrough
      }
    }
    const lines = trimmed.split('\n').map(line => line.trim()).filter(line => line.startsWith('{') || line.startsWith('['));
    for (let i = lines.length - 1; i >= 0; i -= 1) {
      try {
        return JSON.parse(lines[i]);
      } catch {
        // ignore
      }
    }
    return null;
  }

  private extractContent(parsed: any): string | undefined {
    if (!parsed) return undefined;
    if (typeof parsed.content === 'string') return parsed.content;
    if (typeof parsed.response === 'string') return parsed.response;
    if (typeof parsed.result === 'string') return parsed.result;
    if (typeof parsed.output === 'string') return parsed.output;
    const messageContent = parsed.message?.content;
    if (Array.isArray(messageContent)) {
      const texts = messageContent
        .map((block: any) => (typeof block?.text === 'string' ? block.text : ''))
        .filter(Boolean);
      if (texts.length > 0) return texts.join('');
    }
    if (typeof parsed.text === 'string') return parsed.text;
    return undefined;
  }

  private extractTokenUsage(parsed: any): SessionResponse['tokenUsage'] | undefined {
    if (!parsed) return undefined;
    const usage =
      parsed.usage ||
      parsed.token_usage ||
      parsed.tokenUsage ||
      parsed.metadata?.usage ||
      parsed.meta?.usage ||
      parsed.response?.usage ||
      parsed.result?.usage;
    if (!usage) return undefined;
    const inputTokens =
      usage.input_tokens ??
      usage.inputTokens ??
      usage.prompt_tokens ??
      usage.promptTokens ??
      usage.input ??
      usage.prompt;
    const outputTokens =
      usage.output_tokens ??
      usage.outputTokens ??
      usage.completion_tokens ??
      usage.completionTokens ??
      usage.output ??
      usage.completion;
    if (inputTokens === undefined && outputTokens === undefined) return undefined;
    return {
      inputTokens: Number(inputTokens || 0),
      outputTokens: Number(outputTokens || 0),
      cacheReadTokens: usage.cache_read_tokens ?? usage.cacheReadTokens,
    };
  }
}
