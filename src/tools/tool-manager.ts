/**
 * 统一工具管理器
 * 管理所有工具源：MCP、内置工具
 *
 * 内置工具 (source: 'builtin'):
 * - launch-process/read-process/write-process/kill-process/list-processes: 终端运行时
 * - text_editor: 文件编辑
 * - grep_search: 代码搜索
 * - remove_files: 文件删除
 * - web_search: 网络搜索
 * - web_fetch: URL 内容获取
 * - mermaid_diagram: Mermaid 图表渲染
 * - codebase_retrieval: 代码库语义搜索 (ACE)
 * - dispatch_task: 将子任务分配给专业 Worker
 * - plan_mission: 创建多 Worker 协作计划
 * - send_worker_message: 向 Worker 面板发送消息
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
import { OrchestrationExecutor } from './orchestration-executor';
import { logger, LogCategory } from '../logging';
import { PermissionMatrix } from '../types';
import type { SnapshotManager } from '../snapshot-manager';
import type { SkillsManager, InstructionSkillDefinition } from './skills-manager';
import type { MCPToolExecutor } from './mcp-executor';
import type { MCPPromptInfo } from './mcp-manager';

/**
 * 快照执行上下文（标识当前正在执行的 mission/assignment/worker）
 */
export interface SnapshotContext {
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;
}

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
  private orchestrationExecutor: OrchestrationExecutor;

  // 外部工具执行器
  private mcpExecutors: Map<string, ToolExecutor> = new Map();
  private skillExecutor: ToolExecutor | null = null;

  // 缓存和权限
  private toolCache: Map<string, ExtendedToolDefinition> = new Map();
  private permissions: PermissionMatrix;
  private authorizationCallback?: (toolName: string, toolArgs: any) => Promise<boolean>;

  // 快照系统
  private snapshotManager?: SnapshotManager;
  private snapshotContext?: SnapshotContext;

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
    this.orchestrationExecutor = new OrchestrationExecutor();

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

    // 重新注入快照回调（因为执行器被重建了）
    this.injectSnapshotCallbacks();

    // 重新读取 ACE 配置并更新
    const aceConfig = loadAceConfigFromFile();
    this.aceExecutor.updateConfig(workspaceRoot, aceConfig.baseUrl, aceConfig.apiKey);

    this.lspExecutor = new LspExecutor(workspaceRoot);
    this.invalidateCache();
    logger.info('ToolManager workspace root updated', { workspaceRoot }, LogCategory.TOOLS);
  }

  /**
   * 注入 SnapshotManager
   * 在文件写入/删除前自动创建快照，确保精确记录所有变更
   */
  setSnapshotManager(snapshotManager: SnapshotManager): void {
    this.snapshotManager = snapshotManager;
    this.injectSnapshotCallbacks();
    logger.info('ToolManager: SnapshotManager 已注入', undefined, LogCategory.TOOLS);
  }

  /**
   * 设置快照执行上下文
   * 在 Assignment 执行前调用，标识当前正在执行的任务信息
   */
  setSnapshotContext(context: SnapshotContext): void {
    this.snapshotContext = context;
  }

  /**
   * 更新快照上下文的 todoId（在每个 Todo 执行开始时调用）
   * 确保文件变更精确关联到具体的 Todo
   */
  updateSnapshotTodoId(todoId: string): void {
    if (this.snapshotContext) {
      this.snapshotContext.todoId = todoId;
    }
  }

  /**
   * 清除快照执行上下文
   * 在 Assignment 执行后调用
   */
  clearSnapshotContext(): void {
    this.snapshotContext = undefined;
  }

  /**
   * 向 FileExecutor 和 RemoveFilesExecutor 注入快照回调
   */
  private injectSnapshotCallbacks(): void {
    if (!this.snapshotManager) return;

    const snapshotManager = this.snapshotManager;
    const self = this;

    const beforeWriteCallback = (filePath: string) => {
      if (!self.snapshotContext) return;
      try {
        snapshotManager.createSnapshotForMission(
          filePath,
          self.snapshotContext.missionId,
          self.snapshotContext.assignmentId,
          self.snapshotContext.todoId,
          self.snapshotContext.workerId,
          'tool-level-snapshot'
        );
      } catch (error: any) {
        // 快照失败不应阻断工具执行
        logger.warn('ToolManager: 工具级快照创建失败', {
          filePath,
          error: error?.message
        }, LogCategory.TOOLS);
      }
    };

    this.fileExecutor.setBeforeWriteCallback(beforeWriteCallback);
    this.removeFilesExecutor.setBeforeWriteCallback(beforeWriteCallback);
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
    'launch-process',
    'read-process',
    'write-process',
    'kill-process',
    'list-processes',
    'text_editor',
    'grep_search',
    'remove_files',
    'web_search',
    'web_fetch',
    'mermaid_diagram',
    'codebase_retrieval',
    'dispatch_task',
    'plan_mission',
    'send_worker_message',
    'report_progress',
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
      case 'launch-process':
        return await this.executeLaunchProcessTool(toolCall);

      case 'read-process':
        return await this.executeReadProcessTool(toolCall);

      case 'write-process':
        return await this.executeWriteProcessTool(toolCall);

      case 'kill-process':
        return await this.executeKillProcessTool(toolCall);

      case 'list-processes':
        return await this.executeListProcessesTool(toolCall);

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

      case 'dispatch_task':
      case 'plan_mission':
      case 'send_worker_message':
        return await this.orchestrationExecutor.execute(toolCall);

      case 'report_progress':
        return this.executeReportProgressTool(toolCall);

      default:
        return {
          toolCallId: toolCall.id,
          content: `Unknown builtin tool: ${name}`,
          isError: true,
        };
    }
  }

  /**
   * 执行 report_progress 工具 — Worker 主动汇报任务进度
   *
   * 不执行任何实际操作，仅通过 EventEmitter 发出 progress:reported 事件。
   * 上层（MissionDrivenEngine）监听此事件并更新 subTaskCard。
   * context_id 由 Worker LLM 从 prompt 中获取（即 assignmentId），
   * 确保并行 Worker 场景下事件能正确关联到对应的任务。
   */
  private executeReportProgressTool(toolCall: ToolCall): ToolResult {
    const args = toolCall.arguments || {};
    const contextId = args.context_id as string || '';
    const step = args.step as string || '';
    const percentage = typeof args.percentage === 'number' ? args.percentage : undefined;
    const details = args.details as string || undefined;

    logger.info('Tool.report_progress', {
      contextId,
      step,
      percentage,
      details: details?.substring(0, 80),
    }, LogCategory.TOOLS);

    this.emit('progress:reported', {
      contextId,
      step,
      percentage,
      details,
    });

    return {
      toolCallId: toolCall.id,
      content: '进度已汇报',
      isError: false,
    };
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
    if (
      toolName === 'launch-process'
      || toolName === 'read-process'
      || toolName === 'write-process'
      || toolName === 'kill-process'
      || toolName === 'list-processes'
    ) {
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

    // 编排工具默认允许（内部调度不需要额外权限）
    if (toolName === 'dispatch_task' || toolName === 'plan_mission' || toolName === 'send_worker_message') {
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

    // 1-5. 终端运行时工具
    const terminalTools: ToolDefinition[] = [
      {
        name: 'launch-process',
        description: `Launch a shell command in an agent-dedicated terminal. name is required (orchestrator, worker-claude, worker-gemini, worker-codex).

Use wait=true for short commands (build, test, git), wait=false for long-running processes (dev server).

IMPORTANT: If a more specific tool can perform the task, use that tool instead:
- To read files or browse directories: use text_editor (view command), NOT cat/ls/find
- To search code content: use grep_search, NOT grep/rg
- To search the web: use web_search, NOT curl
- To fetch a URL: use web_fetch, NOT curl/wget
- Only use launch-process for commands that truly need a shell (build, test, git, start server, etc.)`,
        input_schema: {
          type: 'object',
          properties: {
            command: { type: 'string', description: '要执行的 shell 命令' },
            cwd: { type: 'string', description: '命令执行目录，相对于工作区根目录（可选，默认为工作区根目录）' },
            wait: { type: 'boolean', description: '是否等待进程完成（默认 true）' },
            max_wait_seconds: { type: 'number', description: '最大等待秒数（默认 30）' },

            name: { type: 'string', description: 'agent 终端名称（必填：orchestrator、worker-claude、worker-gemini、worker-codex）' },
            showTerminal: { type: 'boolean', description: '是否显示终端窗口（默认 true）' },
          },
          required: ['command', 'name'],
        },
      },
      {
        name: 'read-process',
        description: '读取终端进程输出与状态，可选择等待。',
        input_schema: {
          type: 'object',
          properties: {
            terminal_id: { type: 'number', description: 'terminal_id（来自 launch-process）' },
            wait: { type: 'boolean', description: '是否等待状态变化（默认 false）' },
            max_wait_seconds: { type: 'number', description: '最大等待秒数（默认 30）' },
          },
          required: ['terminal_id'],
        },
      },
      {
        name: 'write-process',
        description: '向运行中的终端进程写入 stdin。',
        input_schema: {
          type: 'object',
          properties: {
            terminal_id: { type: 'number', description: 'terminal_id（来自 launch-process）' },
            input_text: { type: 'string', description: '写入终端的文本' },
          },
          required: ['terminal_id', 'input_text'],
        },
      },
      {
        name: 'kill-process',
        description: '终止指定终端进程。',
        input_schema: {
          type: 'object',
          properties: {
            terminal_id: { type: 'number', description: 'terminal_id（来自 launch-process）' },
          },
          required: ['terminal_id'],
        },
      },
      {
        name: 'list-processes',
        description: '列出当前所有终端进程记录。',
        input_schema: {
          type: 'object',
          properties: {},
        },
      },
    ];

    for (const tool of terminalTools) {
      tools.push({
        ...tool,
        metadata: {
          source: 'builtin',
          category: 'system',
          tags: ['shell', 'terminal', 'runtime'],
        },
      });
    }

    // 6. text_editor (文件编辑)
    tools.push(this.fileExecutor.getToolDefinition());

    // 7. grep_search (代码搜索)
    tools.push(this.searchExecutor.getToolDefinition());

    // 8. remove_files (文件删除)
    tools.push(this.removeFilesExecutor.getToolDefinition());

    // 9-10. web_search, web_fetch (网络搜索/获取)
    tools.push(...this.webExecutor.getToolDefinitions());

    // 11. mermaid_diagram (Mermaid 图表)
    tools.push(this.mermaidExecutor.getToolDefinition());

    // 12. codebase_retrieval (ACE 语义搜索)
    tools.push(this.aceExecutor.getToolDefinition());

    // 13-15. 编排工具 (dispatch_task, plan_mission, send_worker_message)
    tools.push(...this.orchestrationExecutor.getToolDefinitions());

    // 16. report_progress (Worker 进度汇报)
    tools.push({
      name: 'report_progress',
      description: '汇报当前任务的执行进度。仅在阶段转换时调用（如：分析完成→开始修改、修改完成→开始验证），不要在每次工具调用后都汇报。一个任务通常汇报 2-4 次即可。',
      input_schema: {
        type: 'object',
        properties: {
          context_id: { type: 'string', description: '任务上下文 ID（从任务说明中获取）' },
          step: { type: 'string', description: '当前正在执行的步骤描述' },
          percentage: { type: 'number', description: '预估完成百分比 0-100' },
          details: { type: 'string', description: '补充细节信息（可选）' },
        },
        required: ['context_id', 'step'],
      },
      metadata: {
        source: 'builtin' as const,
        category: 'system',
        tags: ['progress', 'reporting'],
      },
    });

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

  private async executeLaunchProcessTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const { command, cwd, wait = true, max_wait_seconds = 30, name, showTerminal = true } = args;

    const validation = this.terminalExecutor.validateCommand(command);
    if (!validation.valid) {
      return {
        toolCallId: toolCall.id,
        content: `Command rejected: ${validation.reason}`,
        isError: true,
      };
    }

    const result = await this.terminalExecutor.launchProcess({
      command,
      cwd,
      wait,
      maxWaitSeconds: max_wait_seconds,
      name,
      showTerminal,
    });

    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError: false,
    };
  }

  private async executeReadProcessTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const { terminal_id, wait = false, max_wait_seconds = 30 } = args;

    const result = await this.terminalExecutor.readProcess(terminal_id, wait, max_wait_seconds);
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError: false,
    };
  }

  private async executeWriteProcessTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const { terminal_id, input_text } = args;

    const result = await this.terminalExecutor.writeProcess(terminal_id, input_text);
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError: false,
    };
  }

  private async executeKillProcessTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const { terminal_id } = args;

    const result = await this.terminalExecutor.killProcess(terminal_id);
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError: false,
    };
  }

  private async executeListProcessesTool(toolCall: ToolCall): Promise<ToolResult> {
    const result = this.terminalExecutor.listProcessRecords();
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
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
   * 获取编排工具执行器（用于注入回调处理器）
   */
  getOrchestrationExecutor(): OrchestrationExecutor {
    return this.orchestrationExecutor;
  }

  /**
   * 获取搜索执行器（用于本地上下文检索回退）
   */
  getSearchExecutor(): SearchExecutor {
    return this.searchExecutor;
  }

  /**
   * 获取 LSP 执行器（用于本地上下文检索回退）
   */
  getLspExecutor(): LspExecutor {
    return this.lspExecutor;
  }

  /**
   * 检查 ACE 是否已配置
   */
  isAceConfigured(): boolean {
    return this.aceExecutor.isConfigured();
  }
}
