/**
 * 统一工具管理器
 * 管理所有工具源：MCP、Skill、内置工具
 */

import { EventEmitter } from 'events';
import {
  ToolExecutor,
  ExtendedToolDefinition,
  ToolMetadata,
} from './types';
import { ToolCall, ToolResult, ToolDefinition } from '../llm/types';
import { ShellExecutor } from './shell-executor';
import { VSCodeTerminalExecutor } from './vscode-terminal-executor';
import { logger, LogCategory } from '../logging';
import { PermissionMatrix } from '../types';

/**
 * 工具管理器
 */
export class ToolManager extends EventEmitter implements ToolExecutor {
  private shellExecutor: ShellExecutor;
  private terminalExecutor: VSCodeTerminalExecutor;
  private mcpExecutors: Map<string, ToolExecutor> = new Map();
  private skillExecutors: Map<string, ToolExecutor> = new Map();
  private toolCache: Map<string, ExtendedToolDefinition> = new Map();
  private permissions: PermissionMatrix;
  private authorizationCallback?: (toolName: string, toolArgs: any) => Promise<boolean>;

  constructor(permissions?: PermissionMatrix) {
    super();
    this.shellExecutor = new ShellExecutor();
    this.terminalExecutor = new VSCodeTerminalExecutor();
    this.permissions = permissions || {
      allowEdit: true,
      allowBash: true,
      allowWeb: true,
    };
  }

  /**
   * 设置工具授权回调
   */
  setAuthorizationCallback(callback?: (toolName: string, toolArgs: any) => Promise<boolean>): void {
    this.authorizationCallback = callback;
    logger.info('Tool authorization callback updated', {
      hasCallback: !!callback
    }, LogCategory.TOOLS);
  }

  /**
   * 设置权限矩阵
   */
  setPermissions(permissions: PermissionMatrix): void {
    this.permissions = permissions;
    logger.info('Tool permissions updated', permissions, LogCategory.TOOLS);
  }

  /**
   * 获取当前权限
   */
  getPermissions(): PermissionMatrix {
    return { ...this.permissions };
  }

  /**
   * 注册 MCP 执行器
   */
  registerMCPExecutor(serverId: string, executor: ToolExecutor): void {
    this.mcpExecutors.set(serverId, executor);
    this.invalidateCache();
    logger.info(`Registered MCP executor: ${serverId}`, undefined, LogCategory.TOOLS);
  }

  /**
   * 注销 MCP 执行器
   */
  unregisterMCPExecutor(serverId: string): void {
    this.mcpExecutors.delete(serverId);
    this.invalidateCache();
    logger.info(`Unregistered MCP executor: ${serverId}`, undefined, LogCategory.TOOLS);
  }

  /**
   * 注册 Skill 执行器
   */
  registerSkillExecutor(skillId: string, executor: ToolExecutor): void {
    this.skillExecutors.set(skillId, executor);
    this.invalidateCache();
    logger.info(`Registered Skill executor: ${skillId}`, undefined, LogCategory.TOOLS);
  }

  /**
   * 注销 Skill 执行器
   */
  unregisterSkillExecutor(skillId: string): void {
    this.skillExecutors.delete(skillId);
    this.invalidateCache();
    logger.info(`Unregistered Skill executor: ${skillId}`, undefined, LogCategory.TOOLS);
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    logger.debug('Executing tool call', {
      toolName: toolCall.name,
      toolCallId: toolCall.id,
    }, LogCategory.TOOLS);

    try {
      // 检查授权（包括权限和用户授权）
      const authCheck = await this.checkAuthorization(toolCall);
      if (!authCheck.allowed) {
        logger.warn('Tool execution blocked', {
          toolName: toolCall.name,
          reason: authCheck.reason,
        }, LogCategory.TOOLS);
        return {
          toolCallId: toolCall.id,
          content: `Tool blocked: ${authCheck.reason}`,
          isError: true,
        };
      }

      // 检查是否是内置 Shell 工具
      if (toolCall.name === 'execute_shell' || toolCall.name === 'Bash') {
        return await this.executeShellTool(toolCall);
      }

      // 查找工具定义
      const toolDef = await this.findTool(toolCall.name);
      if (!toolDef) {
        return {
          toolCallId: toolCall.id,
          content: `Tool '${toolCall.name}' not found`,
          isError: true,
        };
      }

      // 根据工具来源执行
      if (toolDef.metadata.source === 'mcp') {
        return await this.executeMCPTool(toolCall, toolDef);
      } else if (toolDef.metadata.source === 'skill') {
        return await this.executeSkillTool(toolCall, toolDef);
      } else {
        return {
          toolCallId: toolCall.id,
          content: `Unknown tool source: ${toolDef.metadata.source}`,
          isError: true,
        };
      }
    } catch (error: any) {
      logger.error('Tool execution failed', {
        toolName: toolCall.name,
        error: error.message,
      }, LogCategory.TOOLS);

      return {
        toolCallId: toolCall.id,
        content: `Tool execution failed: ${error.message}`,
        isError: true,
      };
    }
  }

  /**
   * 检查工具授权（包括权限和用户授权）
   */
  private async checkAuthorization(toolCall: ToolCall): Promise<{ allowed: boolean; reason?: string }> {
    // 1. 先检查基础权限
    const permissionCheck = this.checkPermission(toolCall.name);
    if (!permissionCheck.allowed) {
      return permissionCheck;
    }

    // 2. 如果没有授权回调，默认允许（Auto 模式）
    if (!this.authorizationCallback) {
      return { allowed: true };
    }

    // 3. 请求用户授权（Ask 模式）
    try {
      const allowed = await this.authorizationCallback(toolCall.name, toolCall.arguments);
      if (!allowed) {
        return { allowed: false, reason: 'User denied tool authorization' };
      }
      return { allowed: true };
    } catch (error) {
      return { allowed: false, reason: 'Authorization request failed' };
    }
  }

  /**
   * 检查工具权限
   */
  private checkPermission(toolName: string): { allowed: boolean; reason?: string } {
    // Bash/Shell 工具需要 allowBash 权限
    if (toolName === 'Bash' || toolName === 'execute_shell') {
      if (!this.permissions.allowBash) {
        return { allowed: false, reason: 'Bash execution is disabled' };
      }
      return { allowed: true };
    }

    // Edit/Write 工具需要 allowEdit 权限
    if (toolName === 'Edit' || toolName === 'Write' || toolName === 'NotebookEdit') {
      if (!this.permissions.allowEdit) {
        return { allowed: false, reason: 'File editing is disabled' };
      }
      return { allowed: true };
    }

    // Web 相关工具需要 allowWeb 权限
    if (toolName === 'WebFetch' || toolName === 'WebSearch' || toolName.toLowerCase().includes('web')) {
      if (!this.permissions.allowWeb) {
        return { allowed: false, reason: 'Web access is disabled' };
      }
      return { allowed: true };
    }

    // 其他工具默认允许（Read, Grep, Glob 等只读工具）
    return { allowed: true };
  }

  /**
   * 获取所有工具定义
   */
  async getTools(): Promise<ExtendedToolDefinition[]> {
    // 如果缓存有效，直接返回
    if (this.toolCache.size > 0) {
      return Array.from(this.toolCache.values());
    }

    const tools: ExtendedToolDefinition[] = [];

    // 添加内置 Shell 工具
    const shellTool = this.shellExecutor.getToolDefinition();
    tools.push({
      ...shellTool,
      metadata: {
        source: 'builtin',
        category: 'system',
        tags: ['shell', 'command', 'execution'],
      },
    });

    // 收集所有 MCP 工具
    for (const [serverId, executor] of this.mcpExecutors) {
      try {
        const mcpTools = await executor.getTools();
        tools.push(...mcpTools);
      } catch (error: any) {
        logger.error(`Failed to get tools from MCP server: ${serverId}`, {
          error: error.message,
        }, LogCategory.TOOLS);
      }
    }

    // 收集所有 Skill 工具
    for (const [skillId, executor] of this.skillExecutors) {
      try {
        const skillTools = await executor.getTools();
        tools.push(...skillTools);
      } catch (error: any) {
        logger.error(`Failed to get tools from Skill: ${skillId}`, {
          error: error.message,
        }, LogCategory.TOOLS);
      }
    }

    // 更新缓存
    for (const tool of tools) {
      this.toolCache.set(tool.name, tool);
    }

    logger.info(`Loaded ${tools.length} tools`, {
      builtin: 1,
      mcp: this.mcpExecutors.size,
      skills: this.skillExecutors.size,
    }, LogCategory.TOOLS);

    return tools;
  }

  /**
   * 检查工具是否可用
   */
  async isAvailable(toolName: string): Promise<boolean> {
    if (toolName === 'execute_shell') {
      return true;
    }

    const tool = await this.findTool(toolName);
    return !!tool;
  }

  /**
   * 查找工具定义
   */
  private async findTool(toolName: string): Promise<ExtendedToolDefinition | undefined> {
    // 先检查缓存
    if (this.toolCache.has(toolName)) {
      return this.toolCache.get(toolName);
    }

    // 重新加载所有工具
    const tools = await this.getTools();
    return tools.find((t) => t.name === toolName);
  }

  /**
   * 执行 Shell 工具
   */
  private async executeShellTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const {
      command,
      cwd,
      timeout,
      showTerminal,
      keepTerminalOpen,
      useVSCodeTerminal,
      name
    } = args;

    // 验证命令安全性
    const validation = this.shellExecutor.validateCommand(command);
    if (!validation.valid) {
      return {
        toolCallId: toolCall.id,
        content: `Command rejected: ${validation.reason}`,
        isError: true,
      };
    }

    // 选择执行器：如果指定使用VSCode终端或需要显示终端，则使用终端执行器
    const shouldUseTerminal = useVSCodeTerminal || showTerminal;
    const executor = shouldUseTerminal ? this.terminalExecutor : this.shellExecutor;

    logger.debug('Executing shell command', {
      command,
      executor: shouldUseTerminal ? 'VSCodeTerminal' : 'ChildProcess',
      showTerminal,
      keepTerminalOpen,
    }, LogCategory.TOOLS);

    const result = await executor.execute({
      command,
      cwd,
      timeout,
      name,
      showTerminal,
      keepTerminalOpen,
      useVSCodeTerminal,
    });

    if (result.exitCode !== 0) {
      return {
        toolCallId: toolCall.id,
        content: `Command failed with exit code ${result.exitCode}\nstdout: ${result.stdout}\nstderr: ${result.stderr}`,
        isError: true,
      };
    }

    return {
      toolCallId: toolCall.id,
      content: result.stdout || '(no output)',
      isError: false,
    };
  }

  /**
   * 执行 MCP 工具
   */
  private async executeMCPTool(
    toolCall: ToolCall,
    toolDef: ExtendedToolDefinition
  ): Promise<ToolResult> {
    const serverId = toolDef.metadata.sourceId;
    if (!serverId) {
      return {
        toolCallId: toolCall.id,
        content: 'MCP server ID not found in tool metadata',
        isError: true,
      };
    }

    let executor = this.mcpExecutors.get(serverId);
    if (!executor) {
      if (this.mcpExecutors.size === 1) {
        executor = Array.from(this.mcpExecutors.values())[0];
        logger.warn('MCP executor not found by serverId, falling back to sole executor', {
          serverId,
        }, LogCategory.TOOLS);
      } else {
        return {
          toolCallId: toolCall.id,
          content: `MCP server '${serverId}' not found`,
          isError: true,
        };
      }
    }

    return await executor.execute(toolCall);
  }

  /**
   * 执行 Skill 工具
   */
  private async executeSkillTool(
    toolCall: ToolCall,
    toolDef: ExtendedToolDefinition
  ): Promise<ToolResult> {
    const skillId = toolDef.metadata.sourceId;
    if (!skillId) {
      return {
        toolCallId: toolCall.id,
        content: 'Skill ID not found in tool metadata',
        isError: true,
      };
    }

    let executor = this.skillExecutors.get(skillId);
    if (!executor) {
      if (this.skillExecutors.size === 1) {
        executor = Array.from(this.skillExecutors.values())[0];
        logger.warn('Skill executor not found by skillId, falling back to sole executor', {
          skillId,
        }, LogCategory.TOOLS);
      } else {
        return {
          toolCallId: toolCall.id,
          content: `Skill '${skillId}' not found`,
          isError: true,
        };
      }
    }

    return await executor.execute(toolCall);
  }

  /**
   * 使缓存失效
   */
  private invalidateCache(): void {
    this.toolCache.clear();
  }

  /**
   * 获取统计信息
   */
  getStats() {
    return {
      mcpServers: this.mcpExecutors.size,
      skills: this.skillExecutors.size,
      cachedTools: this.toolCache.size,
    };
  }
}
