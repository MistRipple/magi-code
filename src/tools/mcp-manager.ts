/**
 * MCP 管理器
 * 负责管理 MCP 服务器连接和工具列表
 */

import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { SSEClientTransport } from '@modelcontextprotocol/sdk/client/sse.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { logger, LogCategory } from '../logging';
import { MCPServerConfig } from './types';

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

/**
 * MCP 服务器连接状态
 */
export interface MCPServerStatus {
  id: string;
  name: string;
  connected: boolean;
  toolCount: number;
  error?: string;
}

/**
 * MCP 管理器
 */
export class MCPManager {
  private clients: Map<string, Client> = new Map();
  private tools: Map<string, MCPToolInfo[]> = new Map();
  private prompts: Map<string, MCPPromptInfo[]> = new Map();

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
   *
   * 说明：
   * 1) 不再使用固定 wall-clock 60s 的外层 Promise.race，避免长任务被误杀；
   * 2) 依赖 SDK RequestOptions 的 timeout + resetTimeoutOnProgress + maxTotalTimeout 实现。
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

  /**
   * 连接到 MCP 服务器
   */
  async connectServer(config: MCPServerConfig): Promise<void> {
    try {
      logger.info('Connecting to MCP server', { id: config.id, name: config.name, type: config.type }, LogCategory.TOOLS);

      // 根据传输类型创建 transport
      let transport;

      if (config.type === 'stdio') {
        if (!config.command) {
          throw new Error('MCP server command is required for stdio type');
        }

        // 过滤掉 undefined 值，确保类型正确
        const envVars: Record<string, string> = {};
        for (const [key, value] of Object.entries(process.env)) {
          if (value !== undefined) {
            envVars[key] = value;
          }
        }
        Object.assign(envVars, config.env || {});

        transport = new StdioClientTransport({
          command: config.command,
          args: config.args || [],
          env: envVars,
        });
      } else if (config.type === 'sse') {
        if (!config.url) {
          throw new Error('MCP server url is required for sse type');
        }
        const sseOpts: any = {};
        if (config.headers) {
          sseOpts.requestInit = { headers: config.headers };
          sseOpts.eventSourceInit = { fetch: (url: string | URL, init?: RequestInit) => fetch(url, { ...init, headers: { ...init?.headers as Record<string, string>, ...config.headers } }) };
        }
        transport = new SSEClientTransport(new URL(config.url), sseOpts);
      } else if (config.type === 'streamable-http') {
        if (!config.url) {
          throw new Error('MCP server url is required for streamable-http type');
        }
        // 先尝试 Streamable HTTP，失败后回退到 SSE（MCP 规范要求的客户端行为）
        const httpOpts: any = {};
        if (config.headers) {
          httpOpts.requestInit = { headers: config.headers };
        }
        transport = new StreamableHTTPClientTransport(new URL(config.url), httpOpts);

        try {
          const probeClient = new Client({ name: 'magi', version: '0.1.0' }, { capabilities: {} });
          await this.withTimeout(probeClient.connect(transport), MCPManager.DEFAULT_CONNECT_TIMEOUT_MS, `MCP streamable-http connect timed out`);

          // Streamable HTTP 连接成功，直接使用此 client
          const toolsResponse = await this.withTimeout(
            probeClient.listTools(),
            MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS,
            `MCP listTools timed out`,
          );
          const tools: MCPToolInfo[] = (toolsResponse.tools || []).map((tool: any) => ({
            name: tool.name,
            description: tool.description || '',
            inputSchema: tool.inputSchema || {},
            serverId: config.id,
            serverName: config.name,
          }));

          let prompts: MCPPromptInfo[] = [];
          try {
            const promptsResponse = await this.withTimeout(probeClient.listPrompts(), MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS, `MCP listPrompts timed out`);
            prompts = (promptsResponse.prompts || []).map((prompt: any) => ({
              name: prompt.name,
              description: prompt.description || '',
              arguments: prompt.arguments || [],
              serverId: config.id,
              serverName: config.name,
            }));
          } catch { /* 服务器可能不支持 prompts */ }

          this.clients.set(config.id, probeClient);
          this.tools.set(config.id, tools);
          this.prompts.set(config.id, prompts);

          logger.info('MCP server connected (streamable-http)', { id: config.id, name: config.name, toolCount: tools.length, promptCount: prompts.length }, LogCategory.TOOLS);
          return;
        } catch (httpError: any) {
          logger.info('Streamable HTTP failed, falling back to SSE', { id: config.id, error: httpError.message }, LogCategory.TOOLS);
          const sseOpts: any = {};
          if (config.headers) {
            sseOpts.requestInit = { headers: config.headers };
            sseOpts.eventSourceInit = { fetch: (url: string | URL, init?: RequestInit) => fetch(url, { ...init, headers: { ...init?.headers as Record<string, string>, ...config.headers } }) };
          }
          transport = new SSEClientTransport(new URL(config.url), sseOpts);
        }
      } else {
        throw new Error(`Unsupported MCP server type: ${config.type}`);
      }

      // 创建客户端
      const client = new Client({
        name: 'magi',
        version: '0.1.0',
      }, {
        capabilities: {},
      });

      // 连接
      await this.withTimeout(
        client.connect(transport),
        MCPManager.DEFAULT_CONNECT_TIMEOUT_MS,
        `MCP connect timed out after ${MCPManager.DEFAULT_CONNECT_TIMEOUT_MS}ms`,
      );

      // 获取工具列表
      const toolsResponse = await this.withTimeout(
        client.listTools(),
        MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS,
        `MCP listTools timed out after ${MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS}ms`,
      );
      const tools: MCPToolInfo[] = (toolsResponse.tools || []).map((tool: any) => ({
        name: tool.name,
        description: tool.description || '',
        inputSchema: tool.inputSchema || {},
        serverId: config.id,
        serverName: config.name,
      }));

      // 获取 Prompts 列表（MCP 协议支持的提示词模板）
      let prompts: MCPPromptInfo[] = [];
      try {
        const promptsResponse = await this.withTimeout(
          client.listPrompts(),
          MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS,
          `MCP listPrompts timed out after ${MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS}ms`,
        );
        prompts = (promptsResponse.prompts || []).map((prompt: any) => ({
          name: prompt.name,
          description: prompt.description || '',
          arguments: prompt.arguments || [],
          serverId: config.id,
          serverName: config.name,
        }));
      } catch (error: any) {
        // 某些 MCP 服务器可能不支持 Prompts，忽略错误
        logger.debug('MCP server does not support prompts or listPrompts failed', {
          id: config.id,
          error: error.message,
        }, LogCategory.TOOLS);
      }

      // 保存客户端、工具列表和 Prompts
      this.clients.set(config.id, client);
      this.tools.set(config.id, tools);
      this.prompts.set(config.id, prompts);

      logger.info('MCP server connected', {
        id: config.id,
        name: config.name,
        toolCount: tools.length,
        promptCount: prompts.length,
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to connect MCP server', {
        id: config.id,
        name: config.name,
        error: error.message,
      }, LogCategory.TOOLS);
      throw error;
    }
  }

  /**
   * 断开 MCP 服务器连接
   */
  async disconnectServer(serverId: string): Promise<void> {
    const client = this.clients.get(serverId);
    if (client) {
      try {
        await client.close();
        this.clients.delete(serverId);
        this.tools.delete(serverId);
        this.prompts.delete(serverId);
        logger.info('MCP server disconnected', { id: serverId }, LogCategory.TOOLS);
      } catch (error: any) {
        logger.error('Failed to disconnect MCP server', {
          id: serverId,
          error: error.message,
        }, LogCategory.TOOLS);
      }
    }
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
    const client = this.clients.get(serverId);
    const tools = this.tools.get(serverId) || [];

    if (!client) {
      return null;
    }

    return {
      id: serverId,
      name: serverId,
      connected: true,
      toolCount: tools.length,
    };
  }

  /**
   * 获取所有服务器状态
   */
  getAllServerStatuses(): MCPServerStatus[] {
    const statuses: MCPServerStatus[] = [];
    for (const [serverId, client] of this.clients.entries()) {
      const tools = this.tools.get(serverId) || [];
      statuses.push({
        id: serverId,
        name: serverId,
        connected: true,
        toolCount: tools.length,
      });
    }
    return statuses;
  }

  /**
   * 刷新服务器工具列表
   */
  async refreshServerTools(serverId: string): Promise<MCPToolInfo[]> {
    const client = this.clients.get(serverId);
    if (!client) {
      throw new Error(`MCP server not connected: ${serverId}`);
    }

    try {
      logger.info('Refreshing MCP server tools', { id: serverId }, LogCategory.TOOLS);

      const toolsResponse = await this.withTimeout(
        client.listTools(),
        MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS,
        `MCP listTools timed out after ${MCPManager.DEFAULT_LIST_TOOLS_TIMEOUT_MS}ms`,
      );
      const tools: MCPToolInfo[] = (toolsResponse.tools || []).map((tool: any) => ({
        name: tool.name,
        description: tool.description || '',
        inputSchema: tool.inputSchema || {},
        serverId: serverId,
        serverName: serverId,
      }));

      this.tools.set(serverId, tools);

      logger.info('MCP server tools refreshed', {
        id: serverId,
        toolCount: tools.length,
      }, LogCategory.TOOLS);

      return tools;
    } catch (error: any) {
      logger.error('Failed to refresh MCP server tools', {
        id: serverId,
        error: error.message,
      }, LogCategory.TOOLS);
      throw error;
    }
  }

  /**
   * 调用工具
   */
  async callTool(serverId: string, toolName: string, args: any, signal?: AbortSignal): Promise<any> {
    const client = this.clients.get(serverId);
    if (!client) {
      throw new Error(`MCP server not connected: ${serverId}`);
    }

    const startedAt = Date.now();
    let lastProgressAt = startedAt;
    try {
      logger.info('Calling MCP tool', {
        serverId,
        toolName,
        args,
        idleTimeoutMs: MCPManager.CALL_TOOL_IDLE_TIMEOUT_MS,
        maxTotalTimeoutMs: MCPManager.CALL_TOOL_MAX_TOTAL_TIMEOUT_MS,
      }, LogCategory.TOOLS);
      const result = await client.callTool(
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
              progress,
            }, LogCategory.TOOLS);
          },
        },
      );

      logger.info('MCP tool call completed', {
        serverId,
        toolName,
        elapsedMs: Date.now() - startedAt,
      }, LogCategory.TOOLS);

      return result;
    } catch (error: any) {
      const now = Date.now();
      logger.error('MCP tool call failed', {
        serverId,
        toolName,
        error: error.message,
        elapsedMs: now - startedAt,
        idleForMs: now - lastProgressAt,
        errorCode: error?.code,
      }, LogCategory.TOOLS);
      throw error;
    }
  }

  /**
   * 断开所有服务器
   */
  async disconnectAll(): Promise<void> {
    const serverIds = Array.from(this.clients.keys());
    await Promise.all(serverIds.map(id => this.disconnectServer(id)));
  }
}
