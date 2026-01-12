import { EventEmitter } from 'events';
import { v4 as uuidv4 } from 'uuid';
import type {
  ICLIAdapter,
  CLIType,
  CLIResponse,
  AdapterState,
  AdapterConfig,
  AdapterMessageMeta,
  CLICapabilities,
} from '../types';
import { CLI_CAPABILITIES } from '../types';
import { SessionManager } from '../session/session-manager';
import type { SessionMessage } from '../session/types';

export interface PersistentAdapterConfig extends Omit<AdapterConfig, 'type'> {
  role: 'worker' | 'orchestrator';
  sessionManager: SessionManager;
}

export class PersistentSessionAdapter extends EventEmitter implements ICLIAdapter {
  readonly type: CLIType;
  protected config: AdapterConfig;
  private role: 'worker' | 'orchestrator';
  private sessionManager: SessionManager;
  private _state: AdapterState = 'idle';

  constructor(type: CLIType, config: PersistentAdapterConfig) {
    super();
    this.type = type;
    this.config = {
      ...config,
      type,
    };
    this.role = config.role;
    this.sessionManager = config.sessionManager;

    this.sessionManager.on('output', ({ cli, role, chunk }) => {
      if (cli === this.type && role === this.role) {
        this.emit('output', chunk);
      }
    });
  }

  get state(): AdapterState {
    return this._state;
  }

  get isConnected(): boolean {
    return this._state === 'ready' || this._state === 'busy';
  }

  get isBusy(): boolean {
    return this._state === 'busy';
  }

  get capabilities(): CLICapabilities {
    return CLI_CAPABILITIES[this.type];
  }

  protected setState(state: AdapterState): void {
    if (this._state !== state) {
      this._state = state;
      this.emit('stateChange', state);
    }
  }

  async connect(): Promise<void> {
    this.setState('connecting');
    await this.sessionManager.startSession(this.type, this.role);
    this.setState('ready');
  }

  async disconnect(): Promise<void> {
    await this.sessionManager.stopSession(this.type, this.role);
    this.setState('disconnected');
  }

  async sendMessage(message: string, imagePaths?: string[], meta?: AdapterMessageMeta): Promise<CLIResponse> {
    if (this.isBusy) {
      throw new Error(`${this.type} CLI is busy`);
    }

    this.setState('busy');
    const requestId = uuidv4();
    let finalMessage = message;
    if (imagePaths && imagePaths.length > 0) {
      const imageRefs = imagePaths.map((p, i) => `图片${i + 1}: ${p}`).join('\n');
      finalMessage = `请先读取并分析以下本地图片文件：\n${imageRefs}\n\n然后回答：${message}`;
    }

    const metadata = meta?.intent || meta?.data || meta?.contextSnapshot
      ? {
          intent: meta?.intent,
          contextSnapshot: meta?.contextSnapshot,
          ...meta?.data,
        }
      : undefined;

    const payload: SessionMessage = {
      requestId,
      taskId: meta?.taskId,
      subTaskId: meta?.subTaskId,
      cli: this.type,
      role: this.role,
      content: finalMessage,
      metadata,
    };

    try {
      const response = await this.sessionManager.send(this.type, this.role, payload);
      const cliResponse: CLIResponse = {
        content: response.content,
        done: true,
        raw: response.raw,
        error: response.error,
        tokenUsage: response.tokenUsage,
      };
      this.emit('response', cliResponse);
      return cliResponse;
    } catch (error) {
      const err = error instanceof Error ? error : new Error(String(error));
      this.emit('error', err);
      throw err;
    } finally {
      this.setState('ready');
    }
  }

  async interrupt(): Promise<void> {
    await this.sessionManager.interrupt(this.type, this.role, 'manual_interrupt');
  }
}
