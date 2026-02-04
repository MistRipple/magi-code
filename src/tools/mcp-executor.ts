/**
 * MCP 工具执行器
 * 将 MCPManager 封装为 ToolExecutor 接口
 */

import { logger, LogCategory } from '../logging';
import { ToolExecutor, ExtendedToolDefinition, MCPServerConfig } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { MCPManager, MCPToolInfo, MCPPromptInfo } from './mcp-manager';
import { LLMConfigLoader } from '../llm/config';

/**
 * MCP 工具执行器
 * 封装 MCPManager 并实现 ToolExecutor 接口
 */
export class MCPToolExecutor implements ToolExecutor {
  private mcpManager: MCPManager;
  private initialized: boolean = false;

  constructor() {
    this.mcpManager = new MCPManager();
  }

  /**
   * 初始化：从配置加载并连接所有 MCP 服务器
   */
  async initialize(): Promise<void> {
    if (this.initialized) {
      return;
    }

    try {
      const servers = LLMConfigLoader.loadMCPConfig();

      for (const server of servers) {
        if (server.enabled) {
          try {
            await this.mcpManager.connectServer(server);
            logger.info('MCP 服务器已连接', { id: server.id, name: server.name }, LogCategory.TOOLS);
          } catch (error: any) {
            logger.error('MCP 服务器连接失败', {
              id: server.id,
              name: server.name,
              error: error.message
            }, LogCategory.TOOLS);
            // 继续连接其他服务器
          }
        }
      }

      this.initialized = true;
      logger.info('MCP 执行器初始化完成', {
        serverCount: servers.length,
        connectedTools: this.mcpManager.getAllTools().length
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('MCP 执行器初始化失败', { error: error.message }, LogCategory.TOOLS);
    }
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    // 确保已初始化
    if (!this.initialized) {
      await this.initialize();
    }

    // 查找工具所属的服务器
    const allTools = this.mcpManager.getAllTools();
    const tool = allTools.find(t => t.name === toolCall.name);

    if (!tool) {
      return {
        toolCallId: toolCall.id,
        content: `MCP tool not found: ${toolCall.name}`,
        isError: true,
      };
    }

    try {
      const result = await this.mcpManager.callTool(
        tool.serverId,
        toolCall.name,
        toolCall.arguments
      );

      // 格式化结果
      let content: string;
      if (result.content) {
        if (Array.isArray(result.content)) {
          content = result.content
            .map((block: any) => {
              if (block.type === 'text') {
                return block.text;
              }
              return JSON.stringify(block);
            })
            .join('\n');
        } else if (typeof result.content === 'string') {
          content = result.content;
        } else {
          content = JSON.stringify(result.content);
        }
      } else {
        content = JSON.stringify(result);
      }

      return {
        toolCallId: toolCall.id,
        content,
        isError: result.isError || false,
      };
    } catch (error: any) {
      logger.error('MCP 工具执行失败', {
        toolName: toolCall.name,
        serverId: tool.serverId,
        error: error.message,
      }, LogCategory.TOOLS);

      return {
        toolCallId: toolCall.id,
        content: `MCP tool execution failed: ${error.message}`,
        isError: true,
      };
    }
  }

  /**
   * 获取所有 MCP 工具定义
   */
  async getTools(): Promise<ExtendedToolDefinition[]> {
    // 确保已初始化
    if (!this.initialized) {
      await this.initialize();
    }

    const allTools = this.mcpManager.getAllTools();

    return allTools.map((tool: MCPToolInfo) => ({
      name: tool.name,
      description: tool.description,
      input_schema: tool.inputSchema,
      metadata: {
        source: 'mcp' as const,
        sourceId: tool.serverId,
        category: 'mcp',
        tags: ['mcp', tool.serverName],
      },
    }));
  }

  /**
   * 🔧 同步获取已缓存的 MCP 工具信息
   * 用于构建系统提示时避免异步调用
   */
  getCachedTools(): Array<{ name: string; description: string; serverId: string; serverName: string }> {
    if (!this.initialized) {
      return [];
    }
    const allTools = this.mcpManager.getAllTools();
    return allTools.map((tool: MCPToolInfo) => ({
      name: tool.name,
      description: tool.description,
      serverId: tool.serverId,
      serverName: tool.serverName,
    }));
  }

  /**
   * 获取所有 MCP Prompts（提示词模板）
   */
  getPrompts(): MCPPromptInfo[] {
    if (!this.initialized) {
      return [];
    }
    return this.mcpManager.getAllPrompts();
  }

  /**
   * 检查工具是否可用
   */
  async isAvailable(toolName: string): Promise<boolean> {
    if (!this.initialized) {
      await this.initialize();
    }

    const allTools = this.mcpManager.getAllTools();
    return allTools.some(t => t.name === toolName);
  }

  /**
   * 连接新的 MCP 服务器
   */
  async connectServer(config: MCPServerConfig): Promise<void> {
    await this.mcpManager.connectServer(config);
  }

  /**
   * 断开 MCP 服务器
   */
  async disconnectServer(serverId: string): Promise<void> {
    await this.mcpManager.disconnectServer(serverId);
  }

  /**
   * 刷新服务器工具列表
   */
  async refreshServerTools(serverId: string): Promise<void> {
    await this.mcpManager.refreshServerTools(serverId);
  }

  /**
   * 获取 MCPManager 实例（用于 UI 交互）
   */
  getMCPManager(): MCPManager {
    return this.mcpManager;
  }

  /**
   * 关闭所有连接
   */
  async shutdown(): Promise<void> {
    const statuses = this.mcpManager.getAllServerStatuses();
    for (const status of statuses) {
      try {
        await this.mcpManager.disconnectServer(status.id);
      } catch (error: any) {
        logger.error('MCP 服务器断开失败', { id: status.id, error: error.message }, LogCategory.TOOLS);
      }
    }
    this.initialized = false;
  }
}
