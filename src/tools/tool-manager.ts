/**
 * 统一工具管理器
 * 管理所有工具源：MCP、内置工具
 *
 * 内置工具 (source: 'builtin'):
 * - execute_shell: 终端命令执行
 * - text_editor: 文件编辑
 * - grep_search: 代码搜索
 * - remove_files: 文件删除
 * - web_search: 网络搜索
 * - web_fetch: URL 内容获取
 * - mermaid_diagram: Mermaid 图表渲染
 * - codebase_retrieval: 代码库语义搜索 (ACE)
 * - lsp_query: LSP 代码智能查询
 *
 * ACE 配置来源：~/.multicli/config.json 的 promptEnhance 字段（唯一）
 */

import { EventEmitter } from 'events';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import {
  ToolExecutor,
  ExtendedToolDefinition,
  ToolMetadata,
} from './types';
import { ToolCall, ToolResult, ToolDefinition } from '../llm/types';
import { VSCodeTerminalExecutor } from './vscode-terminal-executor';
import { FileExecutor } from './file-executor';
import { SearchExecutor } from './search-executor';
import { RemoveFilesExecutor } from './remove-files-executor';
import { WebExecutor } from './web-executor';
import { MermaidExecutor } from './mermaid-executor';
import { AceExecutor } from './ace-executor';
import { LspExecutor } from './lsp-executor';
import { logger, LogCategory } from '../logging';
import { PermissionMatrix } from '../types';
import type { SkillsManager, InstructionSkillDefinition } from './skills-manager';
import type { MCPToolExecutor } from './mcp-executor';
import type { MCPPromptInfo } from './mcp-manager';

/**
 * 统一 Prompt 信息接口（MCP Prompts + Instruction Skills）
 */
export interface UnifiedPromptInfo {
  name: string;
  description: string;
  content?: string;  // instructionSkills 有 content，MCP Prompts 没有
  source: 'mcp' | 'skill';
  sourceId?: string;  // MCP server ID 或 skill 来源
  arguments?: Array<{
    name: string;
    description?: string;
    required?: boolean;
  }>;
  // Skill 特有属性
  allowedTools?: string[];
  disableModelInvocation?: boolean;
  userInvocable?: boolean;
  argumentHint?: string;
}

/**
 * 读取 ACE 配置（唯一配置读取入口）
 * 配置存储在 ~/.multicli/config.json 的 promptEnhance 字段
 *
 * 导出供其他模块使用，确保配置读取逻辑唯一
 */
export function loadAceConfigFromFile(): { baseUrl: string; apiKey: string } {
  try {
    const configPath = path.join(os.homedir(), '.multicli', 'config.json');
    if (fs.existsSync(configPath)) {
      const config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
      if (config.promptEnhance) {
        return {
          baseUrl: config.promptEnhance.baseUrl || '',
          apiKey: config.promptEnhance.apiKey || ''
        };
      }
    }
  } catch (error) {
    logger.warn('ToolManager: 读取 ACE 配置失败', { error }, LogCategory.TOOLS);
  }
  return { baseUrl: '', apiKey: '' };
}

/**
 * 工具管理器
 */
export class ToolManager extends EventEmitter implements ToolExecutor {
  // 工作区根目录
  private workspaceRoot: string;

  // 内置工具执行器
  private terminalExecutor: VSCodeTerminalExecutor;
  private fileExecutor: FileExecutor;
  private searchExecutor: SearchExecutor;
  private removeFilesExecutor: RemoveFilesExecutor;
  private webExecutor: WebExecutor;
  private mermaidExecutor: MermaidExecutor;
  private aceExecutor: AceExecutor;
  private lspExecutor: LspExecutor;

  // 外部工具执行器
  private mcpExecutors: Map<string, ToolExecutor> = new Map();
  private skillExecutor: ToolExecutor | null = null;

  // 缓存和权限
  private toolCache: Map<string, ExtendedToolDefinition> = new Map();
  private permissions: PermissionMatrix;
  private authorizationCallback?: (toolName: string, toolArgs: any) => Promise<boolean>;

  constructor(workspaceRoot?: string, permissions?: PermissionMatrix) {
    super();
    this.workspaceRoot = workspaceRoot || process.cwd();

    // 读取 ACE 配置（统一入口）
    const aceConfig = loadAceConfigFromFile();

    // 初始化所有内置执行器
    this.terminalExecutor = new VSCodeTerminalExecutor();
    this.fileExecutor = new FileExecutor(this.workspaceRoot);
    this.searchExecutor = new SearchExecutor(this.workspaceRoot);
    this.removeFilesExecutor = new RemoveFilesExecutor(this.workspaceRoot);
    this.webExecutor = new WebExecutor();
    this.mermaidExecutor = new MermaidExecutor();
    this.aceExecutor = new AceExecutor(this.workspaceRoot, aceConfig.baseUrl, aceConfig.apiKey);
    this.lspExecutor = new LspExecutor(this.workspaceRoot);

    this.permissions = permissions || {
      allowEdit: true,
      allowBash: true,
      allowWeb: true,
    };

    if (aceConfig.baseUrl && aceConfig.apiKey) {
      logger.info('ToolManager: ACE 已配置', { baseUrl: aceConfig.baseUrl }, LogCategory.TOOLS);
    }
  }

  /**
   * 更新工作区路径（重新初始化依赖工作区的执行器）
   */
  setWorkspaceRoot(workspaceRoot: string): void {
    this.workspaceRoot = workspaceRoot;
    this.fileExecutor = new FileExecutor(workspaceRoot);
    this.searchExecutor = new SearchExecutor(workspaceRoot);
    this.removeFilesExecutor = new RemoveFilesExecutor(workspaceRoot);

    // 重新读取 ACE 配置并更新
    const aceConfig = loadAceConfigFromFile();
    this.aceExecutor.updateConfig(workspaceRoot, aceConfig.baseUrl, aceConfig.apiKey);

    this.lspExecutor = new LspExecutor(workspaceRoot);
    this.invalidateCache();
    logger.info('ToolManager workspace root updated', { workspaceRoot }, LogCategory.TOOLS);
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
   * 注册 Skills 执行器（自定义工具 / 指令型 Skills）
   */
  registerSkillExecutor(executor: ToolExecutor): void {
    this.skillExecutor = executor;
    this.invalidateCache();
    logger.info('Registered Skill executor', undefined, LogCategory.TOOLS);
  }

  /**
   * 注销 Skills 执行器
   */
  unregisterSkillExecutor(): void {
    if (this.skillExecutor) {
      this.skillExecutor = null;
      this.invalidateCache();
      logger.info('Unregistered Skill executor', undefined, LogCategory.TOOLS);
    }
  }

  /**
   * 内置工具名称列表
   */
  private readonly builtinToolNames = [
    'execute_shell',
    'Bash',
    'text_editor',
    'grep_search',
    'remove_files',
    'web_search',
    'web_fetch',
    'mermaid_diagram',
    'codebase_retrieval',
    'lsp_query'
  ];

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

      // 检查是否是内置工具
      if (this.builtinToolNames.includes(toolCall.name)) {
        return await this.executeBuiltinTool(toolCall);
      }

      // 查找 MCP 工具
      const toolDef = await this.findTool(toolCall.name);
      if (!toolDef) {
        return {
          toolCallId: toolCall.id,
          content: `Tool '${toolCall.name}' not found`,
          isError: true,
        };
      }

      // 执行 MCP 工具
      if (toolDef.metadata.source === 'mcp') {
        return await this.executeMCPTool(toolCall, toolDef);
      }

      // 执行 Skill 工具
      if (toolDef.metadata.source === 'skill') {
        if (!this.skillExecutor) {
          return {
            toolCallId: toolCall.id,
            content: 'Skill executor not registered',
            isError: true,
          };
        }
        return await this.skillExecutor.execute(toolCall);
      }

      return {
        toolCallId: toolCall.id,
        content: `Unknown tool source: ${toolDef.metadata.source}`,
        isError: true,
      };
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
   * 执行内置工具
   */
  private async executeBuiltinTool(toolCall: ToolCall): Promise<ToolResult> {
    const { name } = toolCall;

    switch (name) {
      case 'execute_shell':
      case 'Bash':
        return await this.executeShellTool(toolCall);

      case 'text_editor':
        return await this.fileExecutor.execute(toolCall);

      case 'grep_search':
        return await this.searchExecutor.execute(toolCall);

      case 'remove_files':
        return await this.removeFilesExecutor.execute(toolCall);

      case 'web_search':
      case 'web_fetch':
        return await this.webExecutor.execute(toolCall);

      case 'mermaid_diagram':
        return await this.mermaidExecutor.execute(toolCall);

      case 'codebase_retrieval':
        return await this.aceExecutor.execute(toolCall);

      case 'lsp_query':
        return await this.lspExecutor.execute(toolCall);

      default:
        return {
          toolCallId: toolCall.id,
          content: `Unknown builtin tool: ${name}`,
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
    // 终端命令工具需要 allowBash 权限（通过 VSCode Terminal 执行）
    if (toolName === 'Bash' || toolName === 'execute_shell') {
      if (!this.permissions.allowBash) {
        return { allowed: false, reason: 'Terminal command execution is disabled' };
      }
      return { allowed: true };
    }

    // Edit/Write 工具需要 allowEdit 权限
    if (toolName === 'Edit' || toolName === 'Write' || toolName === 'NotebookEdit' || toolName === 'text_editor' || toolName === 'remove_files') {
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

    // 添加所有内置工具
    const builtinTools = this.getBuiltinTools();
    tools.push(...builtinTools);

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

    // 收集 Skills 自定义工具
    if (this.skillExecutor) {
      try {
        const skillTools = await this.skillExecutor.getTools();
        tools.push(...skillTools);
      } catch (error: any) {
        logger.error('Failed to get tools from skill executor', {
          error: error.message,
        }, LogCategory.TOOLS);
      }
    }

    // 更新缓存
    for (const tool of tools) {
      this.toolCache.set(tool.name, tool);
    }

    logger.info(`Loaded ${tools.length} tools`, {
      builtin: builtinTools.length,
      mcp: this.mcpExecutors.size,
      skill: this.skillExecutor ? 1 : 0,
    }, LogCategory.TOOLS);

    return tools;
  }

  /**
   * 获取所有内置工具定义
   */
  private getBuiltinTools(): ExtendedToolDefinition[] {
    const tools: ExtendedToolDefinition[] = [];

    // 1. execute_shell (终端命令)
    const shellTool = this.terminalExecutor.getToolDefinition();
    tools.push({
      ...shellTool,
      metadata: {
        source: 'builtin',
        category: 'system',
        tags: ['shell', 'command', 'execution', 'terminal'],
      },
    });

    // 2. text_editor (文件编辑)
    tools.push(this.fileExecutor.getToolDefinition());

    // 3. grep_search (代码搜索)
    tools.push(this.searchExecutor.getToolDefinition());

    // 4. remove_files (文件删除)
    tools.push(this.removeFilesExecutor.getToolDefinition());

    // 5-6. web_search, web_fetch (网络搜索/获取)
    tools.push(...this.webExecutor.getToolDefinitions());

    // 7. mermaid_diagram (Mermaid 图表)
    tools.push(this.mermaidExecutor.getToolDefinition());

    // 8. codebase_retrieval (ACE 语义搜索)
    tools.push(this.aceExecutor.getToolDefinition());

    // 9. lsp_query (LSP 代码智能)
    tools.push(this.lspExecutor.getToolDefinition());

    return tools;
  }

  /**
   * 检查工具是否可用
   */
  async isAvailable(toolName: string): Promise<boolean> {
    // 内置工具始终可用
    if (this.builtinToolNames.includes(toolName)) {
      return true;
    }

    // 检查 MCP 工具
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
   * 执行 Shell 工具（统一使用 VSCode Terminal 实现可视化）
   */
  private async executeShellTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const {
      command,
      cwd,
      timeout,
      showTerminal = true,  // 默认显示终端
      keepTerminalOpen = true,  // 🔧 修复：默认保持终端打开，避免"一闪而过"
      name
    } = args;

    // 验证命令安全性
    const validation = this.terminalExecutor.validateCommand(command);
    if (!validation.valid) {
      return {
        toolCallId: toolCall.id,
        content: `Command rejected: ${validation.reason}`,
        isError: true,
      };
    }

    logger.debug('Executing shell command in VSCode Terminal', {
      command,
      showTerminal,
      keepTerminalOpen,
    }, LogCategory.TOOLS);

    // 统一使用 VSCode Terminal 执行器
    const result = await this.terminalExecutor.execute({
      command,
      cwd,
      timeout,
      name,
      showTerminal,
      keepTerminalOpen,
      useVSCodeTerminal: true,
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
      builtinTools: this.builtinToolNames.length,
      mcpServers: this.mcpExecutors.size,
      cachedTools: this.toolCache.size,
    };
  }

  /**
   * 获取工作区路径
   */
  getWorkspaceRoot(): string {
    return this.workspaceRoot;
  }

  /**
   * 获取所有 Prompts（统一入口：MCP Prompts + Instruction Skills）
   *
   * 这是单一真相来源：所有提示词/指令都通过此方法获取
   */
  getPrompts(): UnifiedPromptInfo[] {
    const prompts: UnifiedPromptInfo[] = [];

    // 1. 收集 MCP Prompts
    for (const [serverId, executor] of this.mcpExecutors) {
      // 检查是否是 MCPToolExecutor
      const mcpExecutor = executor as MCPToolExecutor;
      if (typeof mcpExecutor.getPrompts === 'function') {
        const mcpPrompts = mcpExecutor.getPrompts();
        for (const prompt of mcpPrompts) {
          prompts.push({
            name: prompt.name,
            description: prompt.description,
            source: 'mcp',
            sourceId: prompt.serverId,
            arguments: prompt.arguments,
          });
        }
      }
    }

    // 2. 收集 Instruction Skills
    if (this.skillExecutor) {
      const skillsManager = this.skillExecutor as SkillsManager;
      if (typeof skillsManager.getInstructionSkills === 'function') {
        const skills = skillsManager.getInstructionSkills();
        for (const skill of skills) {
          prompts.push({
            name: skill.name,
            description: skill.description,
            content: skill.content,
            source: 'skill',
            sourceId: skill.repositoryId,
            allowedTools: skill.allowedTools,
            disableModelInvocation: skill.disableModelInvocation,
            userInvocable: skill.userInvocable,
            argumentHint: skill.argumentHint,
          });
        }
      }
    }

    logger.debug('ToolManager.getPrompts', {
      total: prompts.length,
      mcp: prompts.filter(p => p.source === 'mcp').length,
      skill: prompts.filter(p => p.source === 'skill').length,
    }, LogCategory.TOOLS);

    return prompts;
  }

  /**
   * 获取 Skill 执行器（用于 EnvironmentContextProvider）
   */
  getSkillExecutor(): SkillsManager | null {
    return this.skillExecutor as SkillsManager | null;
  }

  /**
   * 获取 MCP 执行器（用于 EnvironmentContextProvider）
   */
  getMCPExecutors(): Map<string, ToolExecutor> {
    return this.mcpExecutors;
  }

  /**
   * 配置 ACE 语义搜索
   */
  configureAce(baseUrl: string, token: string): void {
    this.aceExecutor.updateConfig(this.workspaceRoot, baseUrl, token);
    this.invalidateCache();
    logger.info('ACE configured', { baseUrl }, LogCategory.TOOLS);
  }

  /**
   * 获取 ACE 执行器（用于手动触发索引等操作）
   */
  getAceExecutor(): AceExecutor {
    return this.aceExecutor;
  }

  /**
   * 检查 ACE 是否已配置
   */
  isAceConfigured(): boolean {
    return this.aceExecutor.isConfigured();
  }
}
