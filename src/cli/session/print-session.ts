import { EventEmitter } from 'events';
import { spawn, ChildProcessWithoutNullStreams } from 'child_process';
import type { SessionMessage, SessionProcess, SessionProcessOptions, SessionResponse } from './types';
import type { CLIType } from '../types';

export interface PrintSessionOptions extends SessionProcessOptions {
  cli: CLIType;
  command: string;
  args?: string[];
}

export class PrintSession extends EventEmitter implements SessionProcess {
  readonly cli: CLIType;
  private readonly cwd: string;
  private readonly env: Record<string, string>;
  private readonly idleTimeoutMs: number;
  private readonly command: string;
  private readonly args: string[];
  private alive = false;
  private current?: ChildProcessWithoutNullStreams;

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

  async start(): Promise<void> {
    this.alive = true;
  }

  async stop(): Promise<void> {
    if (this.current) {
      this.current.kill();
      this.current = undefined;
    }
    this.alive = false;
  }

  async interrupt(reason?: string): Promise<void> {
    if (this.current) {
      this.current.kill('SIGINT');
    }
    if (reason) {
      this.emit('log', `[${this.cli}] interrupt: ${reason}`);
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
      if (child.stdin) {
        child.stdin.end();
      }
      let stdout = '';
      let stderr = '';
      const timeoutId = setTimeout(() => {
        child.kill('SIGTERM');
        reject(new Error(`${this.cli} session timeout`));
      }, this.idleTimeoutMs);

      child.stdout.on('data', (data) => {
        const text = data.toString();
        stdout += text;
        this.emit('output', text);
      });
      child.stderr.on('data', (data) => {
        const text = data.toString();
        stderr += text;
        this.emit('output', text);
      });
      child.on('error', (error) => {
        clearTimeout(timeoutId);
        reject(error instanceof Error ? error : new Error(String(error)));
      });
      child.on('close', (code) => {
        clearTimeout(timeoutId);
        this.current = undefined;
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

  private estimateTokens(text: string): number {
    const trimmed = text.trim();
    if (!trimmed) return 0;
    return Math.max(1, Math.ceil(trimmed.length / 4));
  }

  private parseResponse(raw: string): { content?: string; tokenUsage?: SessionResponse['tokenUsage'] } | null {
    const trimmed = raw.trim();
    if (!trimmed) return null;
    const parsed = this.tryParseJson(trimmed);
    if (!parsed) return null;
    const content = this.extractContent(parsed);
    const tokenUsage = this.extractTokenUsage(parsed);
    return { content, tokenUsage };
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
