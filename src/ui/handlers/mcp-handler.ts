/**
 * McpHandler - MCP 服务器管理消息处理器（P1-3 修复）
 *
 * 从 WebviewProvider 提取的独立 Handler。
 * 职责：MCP Server CRUD + 连接管理 + 工具刷新。
 */

import { logger, LogCategory } from '../../logging';
import type { WebviewToExtensionMessage } from '../../types';
import type { CommandHandler, CommandHandlerContext } from './types';

type Msg<T extends string> = Extract<WebviewToExtensionMessage, { type: T }>;

const SUPPORTED = new Set([
  'loadMCPServers', 'addMCPServer', 'updateMCPServer', 'deleteMCPServer',
  'connectMCPServer', 'disconnectMCPServer', 'refreshMCPTools', 'getMCPServerTools',
]);

export class McpCommandHandler implements CommandHandler {
  readonly supportedTypes: ReadonlySet<string> = SUPPORTED;

  async handle(message: WebviewToExtensionMessage, ctx: CommandHandlerContext): Promise<void> {
    switch (message.type) {
      case 'loadMCPServers':
        await this.handleLoadMCPServers(ctx);
        break;
      case 'addMCPServer':
        await this.handleAddMCPServer(message as Msg<'addMCPServer'>, ctx);
        break;
      case 'updateMCPServer':
        await this.handleUpdateMCPServer(message as Msg<'updateMCPServer'>, ctx);
        break;
      case 'deleteMCPServer':
        await this.handleDeleteMCPServer(message as Msg<'deleteMCPServer'>, ctx);
        break;
      case 'connectMCPServer':
        await this.handleConnectMCPServer(message as Msg<'connectMCPServer'>, ctx);
        break;
      case 'disconnectMCPServer':
        await this.handleDisconnectMCPServer(message as Msg<'disconnectMCPServer'>, ctx);
        break;
      case 'refreshMCPTools':
        await this.handleRefreshMCPTools(message as Msg<'refreshMCPTools'>, ctx);
        break;
      case 'getMCPServerTools':
        await this.handleGetMCPServerTools(message as Msg<'getMCPServerTools'>, ctx);
        break;
    }
  }

  private async getMCPManager(ctx: CommandHandlerContext): Promise<any> {
    const executor = ctx.getAdapterFactory().getMCPExecutor();
    if (!executor || typeof (executor as any).getMCPManager !== 'function') {
      throw new Error('MCP executor not available');
    }
    return (executor as any).getMCPManager();
  }

  private async handleLoadMCPServers(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const servers = LLMConfigLoader.loadMCPConfig();
      for (const server of servers) {
        if (!server || typeof server !== 'object') throw new Error('Invalid MCP server entry');
        if (!server.id || typeof server.id !== 'string' || !server.id.trim()) throw new Error('MCP server missing id');
        if (!server.name || typeof server.name !== 'string' || !server.name.trim()) throw new Error(`MCP server ${server.id || '<unknown>'} missing name`);
      }

      let manager: any = null;
      try {
        manager = await this.getMCPManager(ctx);
      } catch {
        // MCP 执行器可能尚未初始化，降级回仅配置态
      }

      const statusMap = new Map<string, any>();
      if (manager && typeof manager.getAllServerStatuses === 'function') {
        for (const status of manager.getAllServerStatuses()) {
          if (status?.id) {
            statusMap.set(status.id, status);
          }
        }
      }

      const mergedServers = servers.map((server: any) => {
        const status = statusMap.get(server.id);
        return {
          ...server,
          connected: status?.connected === true,
          health: status?.health || (status?.connected ? 'connected' : 'disconnected'),
          error: typeof status?.error === 'string' ? status.error : undefined,
          toolCount: Number.isFinite(status?.toolCount) ? status.toolCount : 0,
          reconnectAttempts: Number.isFinite(status?.reconnectAttempts) ? status.reconnectAttempts : 0,
          lastCheckedAt: Number.isFinite(status?.lastCheckedAt) ? status.lastCheckedAt : undefined,
          lastReconnectAt: Number.isFinite(status?.lastReconnectAt) ? status.lastReconnectAt : undefined,
          lastReconnectSuccessfulAt: Number.isFinite(status?.lastReconnectSuccessfulAt) ? status.lastReconnectSuccessfulAt : undefined,
        };
      });

      ctx.sendData('mcpServersLoaded', { servers: mergedServers });
    } catch (error: any) {
      logger.error('加载 MCP 服务器列表失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`加载 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  private async handleAddMCPServer(message: Msg<'addMCPServer'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const server = message.server;

      if (!server || typeof server !== 'object') throw new Error('Invalid MCP server payload');
      if (!server.id || typeof server.id !== 'string' || !server.id.trim()) throw new Error('MCP server missing id');
      if (!server.name || typeof server.name !== 'string' || !server.name.trim()) throw new Error('MCP server missing name');

      LLMConfigLoader.addMCPServer(server);
      ctx.sendData('mcpServerAdded', { server });
      ctx.sendToast(`MCP 服务器 "${server.name}" 已添加`, 'success');

      await ctx.getAdapterFactory().reloadMCP();
      logger.info('MCP 服务器已添加', { id: server.id, name: server.name }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('添加 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`添加 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  private async handleUpdateMCPServer(message: Msg<'updateMCPServer'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateMCPServer(message.serverId, message.updates);
      ctx.sendData('mcpServerUpdated', { serverId: message.serverId });
      ctx.sendToast('MCP 服务器已更新', 'success');
      await ctx.getAdapterFactory().reloadMCP();
      logger.info('MCP 服务器已更新', { id: message.serverId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('更新 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`更新 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  private async handleDeleteMCPServer(message: Msg<'deleteMCPServer'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const manager = await this.getMCPManager(ctx);
      await manager.disconnectServer(message.serverId);
      LLMConfigLoader.deleteMCPServer(message.serverId);
      ctx.sendData('mcpServerDeleted', { serverId: message.serverId });
      ctx.sendToast('MCP 服务器已删除', 'success');
      await ctx.getAdapterFactory().reloadMCP();
      logger.info('MCP 服务器已删除', { id: message.serverId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('删除 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`删除 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  private async handleConnectMCPServer(message: Msg<'connectMCPServer'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const servers = LLMConfigLoader.loadMCPConfig();
      const server = servers.find((s: any) => s.id === message.serverId);
      if (!server) throw new Error(`MCP 服务器不存在: ${message.serverId}`);
      if (!server.enabled) throw new Error('MCP 服务器未启用');

      const manager = await this.getMCPManager(ctx);
      await manager.connectServer(server);
      const tools = manager.getServerTools(message.serverId);
      ctx.sendToast(`MCP 服务器 "${server.name}" 已连接，发现 ${tools.length} 个工具`, 'success');
      await this.handleLoadMCPServers(ctx);
      logger.info('MCP 服务器已连接', { id: message.serverId, toolCount: tools.length }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('连接 MCP 服务器失败', { serverId: message.serverId, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`连接 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  private async handleDisconnectMCPServer(message: Msg<'disconnectMCPServer'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const manager = await this.getMCPManager(ctx);
      await manager.disconnectServer(message.serverId);
      ctx.sendToast('MCP 服务器已断开连接', 'success');
      await this.handleLoadMCPServers(ctx);
      logger.info('MCP 服务器已断开', { id: message.serverId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('断开 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`断开 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  private async handleRefreshMCPTools(message: Msg<'refreshMCPTools'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const manager = await this.getMCPManager(ctx);
      const tools = await manager.refreshServerTools(message.serverId);
      ctx.sendData('mcpToolsRefreshed', { serverId: message.serverId, tools });
      await this.handleLoadMCPServers(ctx);
      ctx.sendToast(`工具列表已刷新，发现 ${tools.length} 个工具`, 'success');
      logger.info('MCP 工具列表已刷新', { serverId: message.serverId, toolCount: tools.length }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('刷新 MCP 工具列表失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`刷新工具列表失败: ${error.message}`, 'error');
    }
  }

  private async handleGetMCPServerTools(message: Msg<'getMCPServerTools'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const manager = await this.getMCPManager(ctx);
      const tools = manager.getServerTools(message.serverId);
      ctx.sendData('mcpServerTools', { serverId: message.serverId, tools });
    } catch (error: any) {
      logger.error('获取 MCP 工具列表失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`获取工具列表失败: ${error.message}`, 'error');
    }
  }
}
