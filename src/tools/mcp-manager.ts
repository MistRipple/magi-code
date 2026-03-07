/**
 * MCP 管理器
 * 负责管理 MCP 服务器连接、健康状态和工具调用
 */

import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { SSEClientTransport } from '@modelcontextprotocol/sdk/client/sse.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { logger, LogCategory } from '../logging';
import { MCPServerConfig } from './types';
import { fetchWithRetry } from './network-utils';

/**
 * MCP 工具信息
 */
export interface MCPToolInfo {
  name: string;
  description: string;
  inputSchema: any;
  serverId: string;
  serverName: string;
}

/**
 * MCP Prompt 信息（提示词模板）
 */
export interface MCPPromptInfo {
  name: string;
  description: string;
  arguments?: Array<{
    name: string;
    description?: string;
    required?: boolean;
  }>;
  serverId: string;
  serverName: string;
}

export type MCPConnectionHealth = 'connected' | 'degraded' | 'disconnected';

/**
 * MCP 服务器连接状态
 */
export interface MCPServerStatus {
  id: string;
  name: string;
  connected: boolean;
  health: MCPConnectionHealth;
  toolCount: number;
  error?: string;
  lastCheckedAt: number;
  reconnectAttempts: number;
  lastReconnectAt?: number;
  lastReconnectSuccessfulAt?: number;
  enabled: boolean;
}

interface MCPServerRuntimeState {
  id: string;
  name: string;
  connected: boolean;
  health: MCPConnectionHealth;
  lastError?: string;
  lastCheckedAt: number;
  reconnectAttempts: number;
  lastReconnectAt?: number;
  lastReconnectSuccessfulAt?: number;
}

interface MCPReconnectResult {
  attempted: boolean;
  success: boolean;
  error?: string;
}

/**
 * MCP 管理器
 */
export class MCPManager {
  private readonly clients: Map<string, Client> = new Map();
  private readonly tools: Map<string, MCPToolInfo[]> = new Map();
  private readonly prompts: Map<string, MCPPromptInfo[]> = new Map();
  private readonly serverConfigs: Map<string, MCPServerConfig> = new Map();
  private readonly serverStates: Map<string, MCPServerRuntimeState> = new Map();
  private readonly reconnectLocks: Map<string, Promise<MCPReconnectResult>> = new Map();

  private static readonly DEFAULT_CONNECT_TIMEOUT_MS = Number(
    process.env.MCP_CONNECT_TIMEOUT_MS || 15000,
  );
  private static readonly DEFAULT_LIST_TOOLS_TIMEOUT_MS = Number(
    process.env.MCP_LIST_TOOLS_TIMEOUT_MS || 15000,
  );
  /**
   * MCP 工具调用超时策略（底层统一策略）
   *
   * - idleTimeout: 距离最近一次进度通知超过该值，判定为“无响应超时”
   * - maxTotalTimeout: 总时长硬上限，防止工具无限占用
   */
  private static readonly CALL_TOOL_IDLE_TIMEOUT_MS = 2 * 60 * 1000;
  private static readonly CALL_TOOL_MAX_TOTAL_TIMEOUT_MS = 30 * 60 * 1000;

  private async withTimeout<T>(
    promise: Promise<T>,
    timeoutMs: number,
    context: string,
  ): Promise<T> {
    let timer: NodeJS.Timeout | null = null;
    try {
      return await Promise.race([
        promise,
        new Promise<T>((_, reject) => {
          timer = setTimeout(() => reject(new Error(context)), timeoutMs);
        }),
      ]);
    } finally {
      if (timer) {
        clearTimeout(timer);
      }
    }
  }

  private cloneConfig(config: MCPServerConfig): MCPServerConfig {
    return {
      ...config,
      args: Array.isArray(config.args) ? [...config.args] : undefined,
      env: config.env ? { ...config.env } : undefined,
      headers: config.headers ? { ...config.headers } : undefined,
    };
  }

  private ensureRuntimeState(serverId: string): MCPServerRuntimeState {
    const existed = this.serverStates.get(serverId);
    if (existed) {
      return existed;
    }

    const cfg = this.serverConfigs.get(serverId);
    const now = Date.now();
    const initial: MCPServerRuntimeState = {
      id: serverId,
      name: cfg?.name || serverId,
      connected: false,
      health: 'disconnected',
      lastCheckedAt: now,
      reconnectAttempts: 0,
    };
    this.serverStates.set(serverId, initial);
    return initial;
  }

  private patchRuntimeState(serverId: string, patch: Partial<MCPServerRuntimeState>): MCPServerRuntimeState {
    const current = this.ensureRuntimeState(serverId);
    const next: MCPServerRuntimeState = {
      ...current,
      ...patch,
      id: serverId,
      name: patch.name ?? current.name,
      lastCheckedAt: patch.lastCheckedAt ?? Date.now(),
    };
    this.serverStates.set(serverId, next);
    return next;
  }

  private toErrorMessage(error: unknown): string {
    if (!error) {
      return 'Unknown error';
    }
    if (error instanceof Error) {
      return error.message || error.name;
    }
    if (typeof error === 'string') {
      return error;
    }
    return String(error);
  }

  private isAbortError(error: unknown): boolean {
    if (!(error instanceof Error)) {
      return false;
    }
    if (error.name === 'AbortError') {
      return true;
    }
    const msg = error.message.toLowerCase();
    return msg.includes('aborted') || msg.includes('中断');
  }

  private isConnectionLikeError(error: unknown): boolean {
    if (this.isAbortError(error)) {
      return false;
    }
    const message = this.toErrorMessage(error).toLowerCase();
    const code = (() => {
      const maybe = error as { code?: unknown } | undefined;
      if (!maybe || maybe.code === undefined || maybe.code === null) {
        return '';
      }
      return String(maybe.code).toLowerCase();
    })();

    if (
      code.includes('econn') ||
      code.includes('enet') ||
      code.includes('socket') ||
      code.includes('pipe')
    ) {
      return true;
    }

    return (
      message.includes('not connected') ||
      message.includes('connection closed') ||
      message.includes('transport closed') ||
      message.includes('socket hang up') ||
      message.includes('econnreset') ||
      message.includes('econnrefused') ||
      message.includes('stream closed') ||
      message.includes('write after end') ||
      message.includes('fetch failed')
    );
  }

  private mergeHeaders(
    baseHeaders?: HeadersInit,
    extraHeaders?: Record<string, string>,
  ): Headers | undefined {
    if (!baseHeaders && !extraHeaders) {
      return undefined;
    }
    const merged = new Headers(baseHeaders);
    for (const [key, value] of Object.entries(extraHeaders || {})) {
      merged.set(key, value);
    }
    return merged;
  }

  private getServerName(serverId: string): string {
    return this.serverStates.get(serverId)?.name
      || this.serverConfigs.get(serverId)?.name
      || serverId;
  }

  private buildStdioTransport(config: MCPServerConfig): StdioClientTransport {
    if (!config.command) {
      throw new Error('MCP server command is required for stdio type');
    }

    const envVars: Record<string, string> = {};
    for (const [key, value] of Object.entries(process.env)) {
      if (value !== undefined) {
        envVars[key] = value;
      }
    }
    Object.assign(envVars, config.env || {});

    return new StdioClientTransport({
      command: config.command,
      args: config.args || [],
      env: envVars,
    });
  }

  private buildSSETransport(config: MCPServerConfig): SSEClientTransport {
    if (!config.url) {
      throw new Error('MCP server url is required for sse type');
    }
    const connectTimeoutMs = Number(process.env.MCP_SSE_CONNECT_TIMEOUT_MS || 15000);
    const connectRetryAttempts = Math.max(1, Number(process.env.MCP_SSE_CONNECT_RETRIES || 2));
    const sseOpts: any = {};
    if (config.headers) {
      sseOpts.requestInit = { headers: config.headers };
    }
    sseOpts.eventSourceInit = {
      fetch: async (url: string | URL, init?: RequestInit) => {
        const mergedHeaders = this.mergeHeaders(init?.headers, config.headers);
        try {
          return await fetchWithRetry(url, {
            ...init,
            headers: mergedHeaders,
          }, {
            timeoutMs: connectTimeoutMs,
            attempts: connectRetryAttempts,
            signal: init?.signal ?? undefined,
          });
        } catch (error) {
          const endpoint = typeof url === 'string' ? url : url.toString();
          const errorMessage = this.toErrorMessage(error);
          throw new Error(`[MCP_SSE_FETCH] ${errorMessage}; url=${endpoint}`);
        }
      },
    };
    return new SSEClientTransport(new URL(config.url), sseOpts);
  }

  private buildStreamableTransport(config: MCPServerConfig): StreamableHTTPClientTransport {
    if (!config.url) {
      throw new Error('MCP server url is required for streamable-http type');
    }
    const requestTimeoutMs = Number(process.env.MCP_STREAMABLE_HTTP_TIMEOUT_MS || 15000);
    const idempotentRetryAttempts = Math.max(1, Number(process.env.MCP_STREAMABLE_HTTP_RETRIES || 2));
    const httpOpts: any = {};
    if (config.headers) {
      httpOpts.requestInit = { headers: config.headers };
    }
    httpOpts.fetch = async (url: string | URL, init?: RequestInit) => {
      const method = (init?.method || 'GET').toUpperCase();
      const isIdempotentMethod = method === 'GET' || method === 'HEAD' || method === 'OPTIONS';
      const attempts = isIdempotentMethod ? idempotentRetryAttempts : 1;
      const mergedHeaders = this.mergeHeaders(init?.headers, config.headers);
      try {
        return await fetchWithRetry(url, {
          ...init,
          headers: mergedHeaders,
        }, {
          timeoutMs: requestTimeoutMs,
          attempts,
          signal: init?.signal ?? undefined,
        });
      } catch (error) {
        const endpoint = typeof url === 'string' ? url : url.toString();
        const errorMessage = this.toErrorMessage(error);
        throw new Error(`[MCP_STREAMABLE_FETCH] ${errorMessage}; method=${method}; url=${endpoint}`);
      }
    };
    return new StreamableHTTPClientTransport(new URL(config.url), httpOpts);
  }

  private async fetchTools(client: Client, config: MCPServerConfig): Promise<MCPToolInfo[]> {
    const toolsResponse = await this.withTimeout(
      client.listTools(),
      MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS,
      `MCP listTools timed out after ${MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS}ms`,
    );
    return (toolsResponse.tools || []).map((tool: any) => ({
      name: tool.name,
      description: tool.description || '',
      inputSchema: tool.inputSchema || {},
      serverId: config.id,
      serverName: config.name,
    }));
  }

  private async fetchPrompts(client: Client, config: MCPServerConfig): Promise<MCPPromptInfo[]> {
    try {
      const promptsResponse = await this.withTimeout(
        client.listPrompts(),
        MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS,
        `MCP listPrompts timed out after ${MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS}ms`,
      );
      return (promptsResponse.prompts || []).map((prompt: any) => ({
        name: prompt.name,
        description: prompt.description || '',
        arguments: prompt.arguments || [],
        serverId: config.id,
        serverName: config.name,
      }));
    } catch (error: any) {
      logger.debug('MCP server does not support prompts or listPrompts failed', {
        id: config.id,
        error: error.message,
      }, LogCategory.TOOLS);
      return [];
    }
  }

  private async connectWithTransport(
    config: MCPServerConfig,
    transport: any,
    transportType: string,
  ): Promise<{ client: Client; tools: MCPToolInfo[]; prompts: MCPPromptInfo[]; transportType: string }> {
    const client = new Client({
      name: 'magi',
      version: '0.1.0',
    }, {
      capabilities: {},
    });

    try {
      await this.withTimeout(
        client.connect(transport),
        MCPManager.DEFAULT_CONNECT_TIMEOUT_MS,
        `MCP connect timed out after ${MCPManager.DEFAULT_CONNECT_TIMEOUT_MS}ms`,
      );

      const tools = await this.fetchTools(client, config);
      const prompts = await this.fetchPrompts(client, config);
      return { client, tools, prompts, transportType };
    } catch (error) {
      try {
        await client.close();
      } catch {
        // 忽略清理失败，保留原始连接错误
      }
      throw error;
    }
  }

  private async closeClientConnection(serverId: string): Promise<void> {
    const client = this.clients.get(serverId);
    if (client) {
      try {
        await client.close();
      } catch (error: any) {
        logger.warn('Failed to close MCP client', {
          id: serverId,
          error: error.message,
        }, LogCategory.TOOLS);
      }
    }

    this.clients.delete(serverId);
    this.tools.delete(serverId);
    this.prompts.delete(serverId);
  }

  private async ensureConnectedClient(serverId: string, reason: string): Promise<Client> {
    const existed = this.clients.get(serverId);
    if (existed) {
      return existed;
    }

    const reconnect = await this.reconnectServer(serverId, reason);
    if (!reconnect.success) {
      throw new Error(`[MCP_RECONNECT] ${reason}; reconnect_failed: ${reconnect.error || 'unknown error'}`);
    }

    const reconnected = this.clients.get(serverId);
    if (!reconnected) {
      throw new Error(`[MCP_RECONNECT] ${reason}; reconnect reported success but client missing`);
    }
    return reconnected;
  }

  private async reconnectServer(serverId: string, reason: string): Promise<MCPReconnectResult> {
    const inFlight = this.reconnectLocks.get(serverId);
    if (inFlight) {
      return inFlight;
    }

    const reconnectTask = (async (): Promise<MCPReconnectResult> => {
      const config = this.serverConfigs.get(serverId);
      if (!config) {
        return {
          attempted: false,
          success: false,
          error: `MCP server config not found: ${serverId}`,
        };
      }

      const prev = this.ensureRuntimeState(serverId);
      const nextAttempt = prev.reconnectAttempts + 1;
      this.patchRuntimeState(serverId, {
        name: config.name,
        connected: false,
        health: 'degraded',
        reconnectAttempts: nextAttempt,
        lastReconnectAt: Date.now(),
      });

      logger.info('MCP reconnect started', {
        id: serverId,
        name: config.name,
        reason,
        attempt: nextAttempt,
      }, LogCategory.TOOLS);

      await this.closeClientConnection(serverId);

      try {
        await this.connectServer(config, { isReconnect: true, reason });
        this.patchRuntimeState(serverId, {
          reconnectAttempts: nextAttempt,
          lastReconnectAt: Date.now(),
          lastReconnectSuccessfulAt: Date.now(),
          connected: true,
          health: 'connected',
          lastError: undefined,
        });
        return { attempted: true, success: true };
      } catch (error) {
        const message = this.toErrorMessage(error);
        this.patchRuntimeState(serverId, {
          reconnectAttempts: nextAttempt,
          connected: false,
          health: 'disconnected',
          lastError: message,
          lastReconnectAt: Date.now(),
        });
        return { attempted: true, success: false, error: message };
      } finally {
        this.reconnectLocks.delete(serverId);
      }
    })();

    this.reconnectLocks.set(serverId, reconnectTask);
    return reconnectTask;
  }

  /**
   * 连接到 MCP 服务器
   */
  async connectServer(
    config: MCPServerConfig,
    options?: { isReconnect?: boolean; reason?: string },
  ): Promise<void> {
    const normalizedConfig = this.cloneConfig(config);
    this.serverConfigs.set(config.id, normalizedConfig);
    this.patchRuntimeState(config.id, {
      name: config.name,
      connected: false,
      health: 'degraded',
      lastError: undefined,
      lastCheckedAt: Date.now(),
    });

    try {
      logger.info('Connecting to MCP server', {
        id: config.id,
        name: config.name,
        type: config.type,
        reconnect: options?.isReconnect === true,
        reason: options?.reason,
      }, LogCategory.TOOLS);

      await this.closeClientConnection(config.id);

      let result: { client: Client; tools: MCPToolInfo[]; prompts: MCPPromptInfo[]; transportType: string };
      if (config.type === 'stdio') {
        const transport = this.buildStdioTransport(config);
        result = await this.connectWithTransport(config, transport, 'stdio');
      } else if (config.type === 'sse') {
        const transport = this.buildSSETransport(config);
        result = await this.connectWithTransport(config, transport, 'sse');
      } else if (config.type === 'streamable-http') {
        const streamable = this.buildStreamableTransport(config);
        try {
          result = await this.connectWithTransport(config, streamable, 'streamable-http');
        } catch (streamableError: any) {
          logger.info('Streamable HTTP failed, falling back to SSE', {
            id: config.id,
            error: streamableError.message,
          }, LogCategory.TOOLS);
          const fallback = this.buildSSETransport(config);
          result = await this.connectWithTransport(config, fallback, 'sse-fallback');
        }
      } else {
        throw new Error(`Unsupported MCP server type: ${config.type}`);
      }

      this.clients.set(config.id, result.client);
      this.tools.set(config.id, result.tools);
      this.prompts.set(config.id, result.prompts);
      this.patchRuntimeState(config.id, {
        name: config.name,
        connected: true,
        health: 'connected',
        lastError: undefined,
        lastCheckedAt: Date.now(),
        ...(options?.isReconnect
          ? { lastReconnectAt: Date.now(), lastReconnectSuccessfulAt: Date.now() }
          : {}),
      });

      logger.info('MCP server connected', {
        id: config.id,
        name: config.name,
        toolCount: result.tools.length,
        promptCount: result.prompts.length,
        transport: result.transportType,
      }, LogCategory.TOOLS);
    } catch (error: any) {
      await this.closeClientConnection(config.id);
      this.patchRuntimeState(config.id, {
        name: config.name,
        connected: false,
        health: 'disconnected',
        lastError: error.message,
        lastCheckedAt: Date.now(),
      });

      logger.error('Failed to connect MCP server', {
        id: config.id,
        name: config.name,
        error: error.message,
        reconnect: options?.isReconnect === true,
      }, LogCategory.TOOLS);
      throw error;
    }
  }

  /**
   * 断开 MCP 服务器连接
   */
  async disconnectServer(serverId: string): Promise<void> {
    await this.closeClientConnection(serverId);
    this.patchRuntimeState(serverId, {
      name: this.getServerName(serverId),
      connected: false,
      health: 'disconnected',
      lastError: undefined,
      lastCheckedAt: Date.now(),
    });
    logger.info('MCP server disconnected', { id: serverId }, LogCategory.TOOLS);
  }

  /**
   * 获取服务器的工具列表
   */
  getServerTools(serverId: string): MCPToolInfo[] {
    return this.tools.get(serverId) || [];
  }

  /**
   * 获取所有工具列表
   */
  getAllTools(): MCPToolInfo[] {
    const allTools: MCPToolInfo[] = [];
    for (const tools of this.tools.values()) {
      allTools.push(...tools);
    }
    return allTools;
  }

  /**
   * 获取服务器的 Prompts 列表
   */
  getServerPrompts(serverId: string): MCPPromptInfo[] {
    return this.prompts.get(serverId) || [];
  }

  /**
   * 获取所有 Prompts 列表
   */
  getAllPrompts(): MCPPromptInfo[] {
    const allPrompts: MCPPromptInfo[] = [];
    for (const prompts of this.prompts.values()) {
      allPrompts.push(...prompts);
    }
    return allPrompts;
  }

  /**
   * 获取服务器连接状态
   */
  getServerStatus(serverId: string): MCPServerStatus | null {
    const hasKnownState = this.serverStates.has(serverId)
      || this.serverConfigs.has(serverId)
      || this.clients.has(serverId);
    if (!hasKnownState) {
      return null;
    }

    const runtime = this.ensureRuntimeState(serverId);
    const cfg = this.serverConfigs.get(serverId);
    const toolCount = (this.tools.get(serverId) || []).length;
    const connected = this.clients.has(serverId) && runtime.connected;
    const health = connected ? 'connected' : runtime.health;

    return {
      id: serverId,
      name: runtime.name || cfg?.name || serverId,
      connected,
      health,
      toolCount,
      error: runtime.lastError,
      lastCheckedAt: runtime.lastCheckedAt,
      reconnectAttempts: runtime.reconnectAttempts,
      lastReconnectAt: runtime.lastReconnectAt,
      lastReconnectSuccessfulAt: runtime.lastReconnectSuccessfulAt,
      enabled: cfg?.enabled !== false,
    };
  }

  /**
   * 获取所有服务器状态
   */
  getAllServerStatuses(): MCPServerStatus[] {
    const ids = new Set<string>([
      ...this.serverConfigs.keys(),
      ...this.serverStates.keys(),
      ...this.clients.keys(),
    ]);
    const statuses: MCPServerStatus[] = [];
    for (const id of ids) {
      const status = this.getServerStatus(id);
      if (status) {
        statuses.push(status);
      }
    }
    return statuses;
  }

  /**
   * 刷新服务器工具列表
   */
  async refreshServerTools(serverId: string): Promise<MCPToolInfo[]> {
    const executeRefresh = async (client: Client): Promise<MCPToolInfo[]> => {
      const config = this.serverConfigs.get(serverId);
      const serverName = config?.name || this.getServerName(serverId);
      const toolsResponse = await this.withTimeout(
        client.listTools(),
        MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS,
        `MCP listTools timed out after ${MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS}ms`,
      );
      const tools: MCPToolInfo[] = (toolsResponse.tools || []).map((tool: any) => ({
        name: tool.name,
        description: tool.description || '',
        inputSchema: tool.inputSchema || {},
        serverId,
        serverName,
      }));
      this.tools.set(serverId, tools);
      return tools;
    };

    const startedAt = Date.now();
    let client = await this.ensureConnectedClient(serverId, `refreshServerTools:${serverId}`);

    try {
      logger.info('Refreshing MCP server tools', { id: serverId }, LogCategory.TOOLS);
      const tools = await executeRefresh(client);
      this.patchRuntimeState(serverId, {
        name: this.getServerName(serverId),
        connected: true,
        health: 'connected',
        lastError: undefined,
        lastCheckedAt: Date.now(),
      });
      logger.info('MCP server tools refreshed', {
        id: serverId,
        toolCount: tools.length,
        elapsedMs: Date.now() - startedAt,
      }, LogCategory.TOOLS);
      return tools;
    } catch (error) {
      const firstError = this.toErrorMessage(error);
      const connectionLike = this.isConnectionLikeError(error);

      logger.error('Failed to refresh MCP server tools', {
        id: serverId,
        error: firstError,
        connectionLike,
      }, LogCategory.TOOLS);

      this.patchRuntimeState(serverId, {
        name: this.getServerName(serverId),
        connected: !connectionLike && this.clients.has(serverId),
        health: connectionLike ? 'degraded' : 'connected',
        lastError: firstError,
        lastCheckedAt: Date.now(),
      });

      if (!connectionLike) {
        throw error;
      }

      const reconnect = await this.reconnectServer(serverId, `refreshServerTools:${serverId}`);
      if (!reconnect.success) {
        throw new Error(`[MCP_RECONNECT] refreshServerTools failed; reconnect_failed: ${reconnect.error || 'unknown error'}`);
      }

      client = await this.ensureConnectedClient(serverId, `refreshServerTools-retry:${serverId}`);
      try {
        const tools = await executeRefresh(client);
        this.patchRuntimeState(serverId, {
          name: this.getServerName(serverId),
          connected: true,
          health: 'connected',
          lastError: undefined,
          lastCheckedAt: Date.now(),
        });
        return tools;
      } catch (retryError) {
        const retryMessage = this.toErrorMessage(retryError);
        this.patchRuntimeState(serverId, {
          name: this.getServerName(serverId),
          connected: false,
          health: 'disconnected',
          lastError: retryMessage,
          lastCheckedAt: Date.now(),
        });
        throw new Error(`[MCP_RECONNECT] refreshServerTools failed after reconnect; ${retryMessage}`);
      }
    }
  }

  /**
   * 调用工具
   */
  async callTool(serverId: string, toolName: string, args: any, signal?: AbortSignal): Promise<any> {
    const startedAt = Date.now();
    let client = await this.ensureConnectedClient(serverId, `callTool:${serverId}:${toolName}`);

    const invoke = async (currentClient: Client, attempt: number): Promise<any> => {
      let lastProgressAt = Date.now();
      logger.info('Calling MCP tool', {
        serverId,
        toolName,
        args,
        attempt,
        idleTimeoutMs: MCPManager.CALL_TOOL_IDLE_TIMEOUT_MS,
        maxTotalTimeoutMs: MCPManager.CALL_TOOL_MAX_TOTAL_TIMEOUT_MS,
      }, LogCategory.TOOLS);

      const result = await currentClient.callTool(
        {
          name: toolName,
          arguments: args,
        },
        undefined,
        {
          signal,
          timeout: MCPManager.CALL_TOOL_IDLE_TIMEOUT_MS,
          resetTimeoutOnProgress: true,
          maxTotalTimeout: MCPManager.CALL_TOOL_MAX_TOTAL_TIMEOUT_MS,
          onprogress: (progress) => {
            lastProgressAt = Date.now();
            logger.debug('MCP tool progress', {
              serverId,
              toolName,
              attempt,
              progress,
            }, LogCategory.TOOLS);
          },
        },
      );

      logger.info('MCP tool call completed', {
        serverId,
        toolName,
        attempt,
        elapsedMs: Date.now() - startedAt,
        idleForMs: Date.now() - lastProgressAt,
      }, LogCategory.TOOLS);
      return result;
    };

    try {
      const result = await invoke(client, 1);
      this.patchRuntimeState(serverId, {
        name: this.getServerName(serverId),
        connected: true,
        health: 'connected',
        lastError: undefined,
        lastCheckedAt: Date.now(),
      });
      return result;
    } catch (firstError) {
      const message = this.toErrorMessage(firstError);
      const connectionLike = this.isConnectionLikeError(firstError);
      logger.error('MCP tool call failed', {
        serverId,
        toolName,
        attempt: 1,
        error: message,
        connectionLike,
      }, LogCategory.TOOLS);

      this.patchRuntimeState(serverId, {
        name: this.getServerName(serverId),
        connected: !connectionLike && this.clients.has(serverId),
        health: connectionLike ? 'degraded' : 'connected',
        lastError: message,
        lastCheckedAt: Date.now(),
      });

      if (!connectionLike) {
        throw firstError;
      }

      for (let retry = 1; retry <= 1; retry += 1) {
        const reconnect = await this.reconnectServer(serverId, `callTool:${serverId}:${toolName}`);
        if (!reconnect.success) {
          throw new Error(`[MCP_RECONNECT] callTool failed; reconnect_attempt=${retry}; reconnect_failed: ${reconnect.error || 'unknown error'}`);
        }

        client = await this.ensureConnectedClient(serverId, `callTool-retry:${serverId}:${toolName}`);
        try {
          const result = await invoke(client, retry + 1);
          this.patchRuntimeState(serverId, {
            name: this.getServerName(serverId),
            connected: true,
            health: 'connected',
            lastError: undefined,
            lastCheckedAt: Date.now(),
          });
          return result;
        } catch (retryError) {
          const retryMessage = this.toErrorMessage(retryError);
          const stillConnectionLike = this.isConnectionLikeError(retryError);
          this.patchRuntimeState(serverId, {
            name: this.getServerName(serverId),
            connected: !stillConnectionLike && this.clients.has(serverId),
            health: stillConnectionLike ? 'disconnected' : 'connected',
            lastError: retryMessage,
            lastCheckedAt: Date.now(),
          });
          throw new Error(`[MCP_RECONNECT] callTool failed after reconnect; reconnect_attempt=${retry}; ${retryMessage}`);
        }
      }

      throw new Error('[MCP_RECONNECT] callTool failed: reconnect retry exhausted');
    }
  }

  /**
   * 断开所有服务器
   */
  async disconnectAll(): Promise<void> {
    const serverIds = Array.from(new Set([
      ...this.clients.keys(),
      ...this.serverConfigs.keys(),
      ...this.serverStates.keys(),
    ]));
    await Promise.all(serverIds.map(id => this.disconnectServer(id)));
  }
}
