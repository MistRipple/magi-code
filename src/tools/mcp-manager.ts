/**
 * MCP 管理器
 * 负责管理 MCP 服务器连接和工具列表
 */

import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
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

  /**
   * 连接到 MCP 服务器
   */
  async connectServer(config: MCPServerConfig): Promise<void> {
    try {
      logger.info('Connecting to MCP server', { id: config.id, name: config.name }, LogCategory.TOOLS);

      if (config.type !== 'stdio') {
        throw new Error(`Unsupported MCP server type: ${config.type}`);
      }

      if (!config.command) {
        throw new Error('MCP server command is required');
      }

      // 创建 stdio 传输
      const transport = new StdioClientTransport({
        command: config.command,
        args: config.args || [],
        env: config.env || {},
      });

      // 创建客户端
      const client = new Client({
        name: 'multicli',
        version: '0.1.0',
      }, {
        capabilities: {},
      });

      // 连接
      await client.connect(transport);

      // 获取工具列表
      const toolsResponse = await client.listTools();
      const tools: MCPToolInfo[] = (toolsResponse.tools || []).map((tool: any) => ({
        name: tool.name,
        description: tool.description || '',
        inputSchema: tool.inputSchema || {},
        serverId: config.id,
        serverName: config.name,
      }));

      // 保存客户端和工具列表
      this.clients.set(config.id, client);
      this.tools.set(config.id, tools);

      logger.info('MCP server connected', {
        id: config.id,
        name: config.name,
        toolCount: tools.length,
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

      const toolsResponse = await client.listTools();
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
  async callTool(serverId: string, toolName: string, args: any): Promise<any> {
    const client = this.clients.get(serverId);
    if (!client) {
      throw new Error(`MCP server not connected: ${serverId}`);
    }

    try {
      logger.info('Calling MCP tool', {
        serverId,
        toolName,
        args,
      }, LogCategory.TOOLS);

      const result = await client.callTool({
        name: toolName,
        arguments: args,
      });

      logger.info('MCP tool call completed', {
        serverId,
        toolName,
      }, LogCategory.TOOLS);

      return result;
    } catch (error: any) {
      logger.error('MCP tool call failed', {
        serverId,
        toolName,
        error: error.message,
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
