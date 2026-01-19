import type { CLIType } from '../types';

export interface SessionMessage {
  requestId: string;
  taskId?: string;
  subTaskId?: string;
  cli: CLIType;
  role: 'orchestrator' | 'worker';
  content: string;
  metadata?: Record<string, unknown>;
  silent?: boolean;
  // Mission-Driven Architecture 扩展
  missionId?: string;
  assignmentId?: string;
  todoId?: string;
}

export interface SessionResponse {
  requestId: string;
  content: string;
  raw: string;
  metadata?: Record<string, unknown>;
  error?: string;
  tokenUsage?: {
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens?: number;
  };
  // Mission-Driven Architecture 扩展
  missionId?: string;
  assignmentId?: string;
  todoId?: string;
}

export interface SessionProcessOptions {
  cli?: CLIType;
  cwd: string;
  env?: Record<string, string>;
  idleTimeoutMs?: number;
  heartbeatMs?: number;
  maxOutputChars?: number;
  commandOverrides?: Partial<Record<CLIType, string>>;
}

/**
 * 会话进程事件类型
 */
export type SessionProcessEvent =
  | 'output'
  | 'question'
  | 'questionTimeout'
  | 'log'
  | 'exit'
  | 'close'
  | 'error'
  | 'stderr';

export interface SessionProcess {
  readonly cli: CLIType;
  readonly isAlive: boolean;
  /** 是否正在等待用户回答 CLI 询问 */
  readonly isWaitingForAnswer: boolean;
  start(): Promise<void>;
  stop(): Promise<void>;
  send(message: SessionMessage): Promise<SessionResponse>;
  interrupt(reason?: string): Promise<void>;
  /** 向 CLI 发送用户输入（用于回答 CLI 询问） */
  writeInput(text: string): boolean;
  /** 事件监听 */
  on(event: SessionProcessEvent, listener: (...args: unknown[]) => void): this;
  /** 移除事件监听 */
  off?(event: SessionProcessEvent, listener: (...args: unknown[]) => void): this;
  /** 触发事件 */
  emit?(event: SessionProcessEvent, ...args: unknown[]): boolean;
}
