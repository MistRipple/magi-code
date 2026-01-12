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

export interface SessionProcess {
  readonly cli: CLIType;
  readonly isAlive: boolean;
  start(): Promise<void>;
  stop(): Promise<void>;
  send(message: SessionMessage): Promise<SessionResponse>;
  interrupt(reason?: string): Promise<void>;
}
