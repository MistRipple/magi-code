/**
 * 统一工具管理器
 * 管理所有工具源：MCP、内置工具
 *
 * 内置工具 (source: 'builtin'):
 * - launch-process/read-process/write-process/kill-process/list-processes: 终端运行时
 * - file_view/file_create/file_edit/file_insert: 文件操作
 * - grep_search: 代码搜索
 * - file_remove: 文件删除
 * - web_search: 网络搜索
 * - web_fetch: URL 内容获取
 * - mermaid_diagram: Mermaid 图表渲染
 * - codebase_retrieval: 代码库语义搜索 (ACE)
 * - dispatch_task: 将子任务分配给专业 Worker
 * - send_worker_message: 向 Worker 面板发送消息
 * - wait_for_workers: 等待 Worker 完成并获取结果（反应式编排）
 * - get_todos: 获取当前任务的 todo 列表
 * - update_todo: 更新 todo 状态
 *
 * ACE 配置来源：~/.magi/config.json 的 promptEnhance 字段（唯一）
 */

import { EventEmitter } from 'events';
import { AsyncLocalStorage } from 'async_hooks';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import {
  ToolExecutor,
  ExtendedToolDefinition,
  ToolMetadata,
  ProcessRunMode,
  BUILTIN_TOOL_NAMES,
} from './types';
import { ToolCall, ToolResult, ToolDefinition, StandardizedToolResult } from '../llm/types';
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
import { WorkspaceFolderInfo, WorkspaceRoots } from '../workspace/workspace-roots';
import { FileMutex } from '../utils/file-mutex';

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
 * 工具执行上下文（按每次工具调用隔离）
 * 用于并行 Worker 下精确绑定 workerId，避免上下文串扰。
 */
export interface ToolExecutionContext {
  workerId?: string;
  role?: 'orchestrator' | 'worker';
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

export interface ToolManagerOptions {
  workspaceRoot?: string;
  workspaceFolders?: WorkspaceFolderInfo[];
  permissions?: PermissionMatrix;
}

/**
 * 读取 ACE 配置（唯一配置读取入口）
 * 配置存储在 ~/.magi/config.json 的 promptEnhance 字段
 *
 * 导出供其他模块使用，确保配置读取逻辑唯一
 */
export function loadAceConfigFromFile(): { baseUrl: string; apiKey: string } {
  try {
    const configPath = path.join(os.homedir(), '.magi', 'config.json');
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
  // 工作区根目录（主目录）+ 多根解析器
  private workspaceRoot: string;
  private workspaceRoots: WorkspaceRoots;

  // 跨执行器共享的文件级互斥锁
  private fileMutex = new FileMutex();

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
  private snapshotContextMap: Map<string, SnapshotContext> = new Map();
  private executionContextStorage = new AsyncLocalStorage<ToolExecutionContext>();
  /** 外部命令可能改动文件时递增，用于判断 file_view 新鲜度 */
  private workspaceMutationEpoch = 0;
  /** 文件路径最近一次 file_view/file_edit/file_insert/file_create 对齐到的 epoch */
  private fileContextEpochByPath: Map<string, number> = new Map();

  constructor(options: ToolManagerOptions = {}) {
    super();
    const root = options.workspaceRoot || process.cwd();
    const folders = options.workspaceFolders && options.workspaceFolders.length > 0
      ? options.workspaceFolders
      : [{ name: path.basename(root), path: root }];

    this.workspaceRoots = new WorkspaceRoots(folders);
    this.workspaceRoot = this.workspaceRoots.getPrimaryFolder().path;

    // 读取 ACE 配置（统一入口）
    const aceConfig = loadAceConfigFromFile();

    // 初始化所有内置执行器（共享 fileMutex 保证文件读写与终端命令的并发安全）
    this.terminalExecutor = new VSCodeTerminalExecutor(this.fileMutex);
    this.fileExecutor = new FileExecutor(this.workspaceRoots, this.fileMutex);
    this.searchExecutor = new SearchExecutor(this.workspaceRoots);
    this.removeFilesExecutor = new RemoveFilesExecutor(this.workspaceRoots);
    this.webExecutor = new WebExecutor();
    this.mermaidExecutor = new MermaidExecutor();
    this.aceExecutor = new AceExecutor(this.workspaceRoot, aceConfig.baseUrl, aceConfig.apiKey);
    this.lspExecutor = new LspExecutor(this.workspaceRoot);
    this.orchestrationExecutor = new OrchestrationExecutor();

    this.permissions = options.permissions || {
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
    this.setWorkspaceFolders([{ name: path.basename(workspaceRoot), path: workspaceRoot }]);
  }

  /**
   * 更新工作区目录（重新初始化依赖工作区的执行器）
   */
  setWorkspaceFolders(workspaceFolders: WorkspaceFolderInfo[]): void {
    this.workspaceRoots = new WorkspaceRoots(workspaceFolders);
    this.workspaceRoot = this.workspaceRoots.getPrimaryFolder().path;
    this.workspaceMutationEpoch = 0;
    this.fileContextEpochByPath.clear();

    // 原地更新，不重建实例（保留 llmEditHandler、onBeforeWrite 等已注册回调）
    this.fileExecutor.updateWorkspaceRoots(this.workspaceRoots);
    this.searchExecutor = new SearchExecutor(this.workspaceRoots);
    this.removeFilesExecutor = new RemoveFilesExecutor(this.workspaceRoots);

    // 重新注入快照回调（SearchExecutor/RemoveFilesExecutor 被重建了）
    this.injectSnapshotCallbacks();

    // 重新读取 ACE 配置并更新
    const aceConfig = loadAceConfigFromFile();
    this.aceExecutor.updateConfig(this.workspaceRoot, aceConfig.baseUrl, aceConfig.apiKey);

    this.lspExecutor = new LspExecutor(this.workspaceRoot);
    this.invalidateCache();
    logger.info('ToolManager workspace roots updated', {
      rootCount: workspaceFolders.length,
      primaryRoot: this.workspaceRoot,
    }, LogCategory.TOOLS);
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
   * 以 workerId 为 key 存储，支持多 Worker 并行执行
   */
  setSnapshotContext(context: SnapshotContext): void {
    this.snapshotContextMap.set(context.workerId, context);
  }

  /**
   * 更新快照上下文的 todoId（在每个 Todo 执行开始时调用）
   * 确保文件变更精确关联到具体的 Todo
   */
  updateSnapshotTodoId(workerId: string, todoId: string): void {
    const context = this.snapshotContextMap.get(workerId);
    if (context) {
      context.todoId = todoId;
    }
  }

  /**
   * 清除快照执行上下文
   * 在 Assignment 执行后调用，按 workerId 精确清除
   */
  clearSnapshotContext(workerId: string): void {
    this.snapshotContextMap.delete(workerId);
  }

  /**
   * 获取当前调用链的执行上下文 workerId
   */
  private getExecutionWorkerId(): string | undefined {
    const workerId = this.executionContextStorage.getStore()?.workerId;
    if (typeof workerId !== 'string') return undefined;
    const normalized = workerId.trim();
    return normalized || undefined;
  }

  /**
   * 获取当前活跃的快照上下文
   * 优先按当前工具执行上下文（workerId）精确定位。
   */
  private getActiveSnapshotContext(): SnapshotContext | undefined {
    if (this.snapshotContextMap.size === 0) return undefined;

    const executionWorkerId = this.getExecutionWorkerId();
    if (executionWorkerId) {
      const scoped = this.snapshotContextMap.get(executionWorkerId);
      if (!scoped) {
        logger.debug('ToolManager: 执行上下文未命中快照上下文', {
          executionWorkerId,
          contextWorkers: Array.from(this.snapshotContextMap.keys()),
        }, LogCategory.TOOLS);
      }
      return scoped;
    }

    logger.debug('ToolManager: 缺少执行上下文，拒绝猜测活跃 worker', {
      contextWorkers: Array.from(this.snapshotContextMap.keys()),
    }, LogCategory.TOOLS);
    return undefined;
  }

  /**
   * 向 FileExecutor 和 RemoveFilesExecutor 注入快照回调
   */
  private injectSnapshotCallbacks(): void {
    if (!this.snapshotManager) return;

    const snapshotManager = this.snapshotManager;
    const self = this;

    const beforeWriteCallback = (filePath: string) => {
      // 从 Map 中获取活跃的快照上下文（多 Worker 并行安全）
      const context = self.getActiveSnapshotContext();
      if (!context) {
        logger.debug('ToolManager: 无活跃快照上下文，跳过快照', { filePath }, LogCategory.TOOLS);
        return;
      }
      try {
        snapshotManager.createSnapshotForMission(
          filePath,
          context.missionId,
          context.assignmentId,
          context.todoId,
          context.workerId,
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
   * 注册由外部提供的大模型文件编辑回调
   * 透传给 FileExecutor（FileExecutor 实例不再重建，回调持久有效）
   */
  setLlmEditHandler(handler: (filePath: string, fileContent: string, summary: string, detailedDesc: string) => Promise<string>): void {
    this.fileExecutor.setLlmEditHandler(handler);
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
   * 内置工具名称列表（引用公共常量 BUILTIN_TOOL_NAMES）
   */
  private readonly builtinToolNames: readonly string[] = BUILTIN_TOOL_NAMES;

  /**
   * 内置工具别名映射（兼容不同命名风格，统一到规范名）
   */
  private readonly builtinToolAliases = new Map<string, string>([
    ['launch_process', 'launch-process'],
    ['read_process', 'read-process'],
    ['write_process', 'write-process'],
    ['kill_process', 'kill-process'],
    ['list_processes', 'list-processes'],
    ['file-view', 'file_view'],
    ['file-create', 'file_create'],
    ['file-edit', 'file_edit'],
    ['file-insert', 'file_insert'],
    ['file-bulk-edit', 'file_bulk_edit'],
    ['file-remove', 'file_remove'],
    ['grep-search', 'grep_search'],
    ['web-search', 'web_search'],
    ['web-fetch', 'web_fetch'],
    ['mermaid-diagram', 'mermaid_diagram'],
    ['codebase-retrieval', 'codebase_retrieval'],
    ['dispatch-task', 'dispatch_task'],
    ['send-worker-message', 'send_worker_message'],
    ['wait-for-workers', 'wait_for_workers'],
    ['split-todo', 'split_todo'],
    ['get-todos', 'get_todos'],
    ['update-todo', 'update_todo'],
  ]);

  /**
   * 仅对高风险（会产生副作用）的工具进行用户授权。
   * 只读查询类工具默认不弹授权，减少 Ask 模式噪音。
   */
  private readonly authorizationRequiredToolNames = new Set<string>([
    'launch-process',
    'write-process',
    'kill-process',
    'file_create',
    'file_edit',
    'file_insert',
    'file_bulk_edit',
    'file_remove',
  ]);

  private normalizeToolName(name: string): string {
    return this.builtinToolAliases.get(name) || name;
  }

  /**
   * 判断工具是否属于高风险副作用操作（需要用户授权）
   */
  requiresUserAuthorization(toolName: string): boolean {
    const normalized = this.normalizeToolName(toolName);
    return this.authorizationRequiredToolNames.has(normalized);
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall, signal?: AbortSignal, executionContext?: ToolExecutionContext): Promise<ToolResult> {
    const run = async (): Promise<ToolResult> => {
      const normalizedName = this.normalizeToolName(toolCall.name);
      const normalizedToolCall = normalizedName === toolCall.name
        ? toolCall
        : { ...toolCall, name: normalizedName };
      let resolvedSource: StandardizedToolResult['source'] = 'builtin';
      let resolvedSourceId: string | undefined;
      const finalize = (raw: ToolResult): ToolResult =>
        this.standardizeToolResult(normalizedToolCall, raw, resolvedSource, resolvedSourceId);

      if (normalizedToolCall !== toolCall) {
        logger.debug('Tool name normalized', {
          originalName: toolCall.name,
          normalizedName: normalizedToolCall.name,
          toolCallId: toolCall.id,
        }, LogCategory.TOOLS);
      }

      logger.debug('Executing tool call', {
        toolName: normalizedToolCall.name,
        toolCallId: toolCall.id,
      }, LogCategory.TOOLS);

      try {
        // 中断检查：执行前检测 abort 信号
        if (signal?.aborted) {
          return finalize({
            toolCallId: toolCall.id,
            content: '任务已中断',
            isError: true,
          });
        }

        // 检查授权（包括权限和用户授权）
        const authCheck = await this.checkAuthorization(normalizedToolCall);
        if (!authCheck.allowed) {
          logger.warn('Tool execution blocked', {
            toolName: normalizedToolCall.name,
            reason: authCheck.reason,
          }, LogCategory.TOOLS);
          return finalize({
            toolCallId: toolCall.id,
            content: `Tool blocked: ${authCheck.reason}`,
            isError: true,
          });
        }

        // 检查是否是内置工具
        if (this.builtinToolNames.includes(normalizedToolCall.name)) {
          resolvedSource = 'builtin';
          return finalize(await this.executeBuiltinTool(normalizedToolCall, signal));
        }

        // 查找 MCP 工具
        const toolDef = await this.findTool(normalizedToolCall.name);
        if (!toolDef) {
          const available = this.builtinToolNames.join(', ');
          return finalize({
            toolCallId: toolCall.id,
            content: `工具 '${toolCall.name}' 不存在。只能使用以下工具: ${available}。请直接使用已有工具完成任务。`,
            isError: true,
          });
        }
        resolvedSource = toolDef.metadata.source;
        resolvedSourceId = toolDef.metadata.sourceId;

        // 执行 MCP 工具
        if (toolDef.metadata.source === 'mcp') {
          return finalize(await this.executeMCPTool(normalizedToolCall, toolDef, signal));
        }

        // 执行内置工具（按定义来源兜底分发，避免白名单漂移导致误判）
        if (toolDef.metadata.source === 'builtin') {
          const builtinCall = toolDef.name === normalizedToolCall.name
            ? normalizedToolCall
            : { ...normalizedToolCall, name: toolDef.name };
          return finalize(await this.executeBuiltinTool(builtinCall, signal));
        }

        // 执行 Skill 工具
        if (toolDef.metadata.source === 'skill') {
          if (!this.skillExecutor) {
            return finalize({
              toolCallId: toolCall.id,
              content: 'Skill executor not registered',
              isError: true,
            });
          }
          return finalize(await this.skillExecutor.execute(normalizedToolCall, signal));
        }

        return finalize({
          toolCallId: toolCall.id,
          content: `Unknown tool source: ${toolDef.metadata.source}`,
          isError: true,
        });
      } catch (error: any) {
        logger.error('Tool execution failed', {
          toolName: normalizedToolCall.name,
          error: error.message,
        }, LogCategory.TOOLS);

        return finalize({
          toolCallId: toolCall.id,
          content: `Tool execution failed: ${error.message}`,
          isError: true,
        });
      }
    };

    if (executionContext) {
      return this.executionContextStorage.run(executionContext, run);
    }
    return run();
  }

  /**
   * 统一标准化工具结果（builtin/mcp/skill 单一出口）
   */
  private standardizeToolResult(
    toolCall: ToolCall,
    raw: ToolResult,
    source: StandardizedToolResult['source'],
    sourceId?: string,
  ): ToolResult {
    const message = typeof raw.content === 'string' ? raw.content : String(raw.content ?? '');
    const parsedData = this.tryParseToolResultData(message);
    const status = this.inferToolResultStatus(raw, message, parsedData);
    const isError = status !== 'success';
    const standardized: StandardizedToolResult = {
      schemaVersion: 'tool-result.v1',
      source,
      sourceId,
      toolName: toolCall.name,
      toolCallId: raw.toolCallId || toolCall.id,
      status,
      message,
      data: parsedData,
      errorCode: status === 'success' ? undefined : this.inferToolErrorCode(status, message, parsedData),
    };

    return {
      ...raw,
      toolCallId: raw.toolCallId || toolCall.id,
      content: message,
      isError,
      standardized,
    };
  }

  private tryParseToolResultData(content: string): unknown | undefined {
    const trimmed = content.trim();
    if (!trimmed || (trimmed[0] !== '{' && trimmed[0] !== '[')) {
      return undefined;
    }
    try {
      return JSON.parse(trimmed);
    } catch {
      return undefined;
    }
  }

  private inferToolResultStatus(
    raw: ToolResult,
    content: string,
    parsedData: unknown,
  ): StandardizedToolResult['status'] {
    const lower = content.toLowerCase();
    const parsed = parsedData && typeof parsedData === 'object'
      ? parsedData as Record<string, unknown>
      : undefined;
    const parsedStatus = typeof parsed?.status === 'string' ? parsed.status.toLowerCase() : '';

    if (content.startsWith('Tool blocked:')) {
      return 'blocked';
    }
    if (content.startsWith('Command rejected:')) {
      return 'rejected';
    }
    if (parsedStatus === 'blocked') {
      return 'blocked';
    }
    if (parsedStatus === 'rejected') {
      return 'rejected';
    }

    const hasFailurePrefix = lower.startsWith('error:')
      || lower.startsWith('[error]')
      || lower.startsWith('mcp tool execution failed:')
      || lower.startsWith('tool execution failed:');
    const hasFailurePayload = parsedStatus === 'error'
      || parsedStatus === 'failed'
      || parsedStatus === 'failure'
      || (typeof parsed?.success === 'boolean' && parsed.success === false)
      || (typeof parsed?.ok === 'boolean' && parsed.ok === false)
      || (typeof parsed?.error === 'string' && parsed.error.trim().length > 0)
      || (typeof parsed?.error_message === 'string' && parsed.error_message.trim().length > 0);
    const hasFailureSignal = Boolean(raw.isError) || hasFailurePrefix || hasFailurePayload;

    if (parsedStatus === 'timeout') {
      return 'timeout';
    }
    if (parsedStatus === 'killed') {
      return 'killed';
    }
    if (parsedStatus === 'aborted') {
      return 'aborted';
    }

    // 仅在已存在失败信号时，才允许关键字将结果细分为 timeout/killed/aborted。
    // 防止 file_view 等“正文包含 timeout/aborted 单词”的正常输出被误判。
    if (hasFailureSignal && (lower.includes(' timed out') || /\btimeout\b/.test(lower) || lower.includes('超时'))) {
      return 'timeout';
    }
    if (hasFailureSignal && (lower.includes('"status":"killed"') || lower.includes('"killed":true') || /\bkilled\b/.test(lower))) {
      return 'killed';
    }
    if (hasFailureSignal && (lower.includes('aborterror') || /\baborted\b/.test(lower) || lower.includes('任务已中断'))) {
      return 'aborted';
    }

    if (hasFailureSignal) {
      return 'error';
    }
    if (parsedStatus === 'success' || parsedStatus === 'completed' || parsedStatus === 'ok') {
      return 'success';
    }
    return 'success';
  }

  private inferToolErrorCode(
    status: StandardizedToolResult['status'],
    content: string,
    parsedData: unknown,
  ): string {
    const parsed = parsedData && typeof parsedData === 'object'
      ? parsedData as Record<string, unknown>
      : undefined;
    const parsedStatus = typeof parsed?.status === 'string' ? parsed.status.toLowerCase() : '';

    if (parsedStatus) {
      return `tool_${parsedStatus}`;
    }
    if (status === 'rejected') {
      return 'tool_rejected';
    }
    if (status === 'blocked') {
      return 'tool_blocked';
    }
    if (status === 'timeout') {
      return 'tool_timeout';
    }
    if (status === 'killed') {
      return 'tool_killed';
    }
    if (status === 'aborted') {
      return 'tool_aborted';
    }
    if (content.includes('[FILE_CONTEXT_STALE]')) {
      return 'file_context_stale';
    }

    if (content.startsWith('MCP tool execution failed:')) {
      return 'mcp_execution_failed';
    }
    if (content.startsWith('Tool execution failed:')) {
      return 'tool_execution_failed';
    }
    return 'tool_error';
  }

  /**
   * 执行内置工具
   */
  private async executeBuiltinTool(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
    const { name } = toolCall;

    switch (name) {
      case 'launch-process':
        return await this.executeLaunchProcessTool(toolCall, signal);

      case 'read-process':
        return await this.executeReadProcessTool(toolCall, signal);

      case 'write-process':
        return await this.executeWriteProcessTool(toolCall);

      case 'kill-process':
        return await this.executeKillProcessTool(toolCall);

      case 'list-processes':
        return await this.executeListProcessesTool(toolCall);

      case 'file_view':
      case 'file_create':
      case 'file_edit':
      case 'file_insert':
      case 'file_bulk_edit': {
        if (name === 'file_edit' || name === 'file_insert') {
          const staleError = this.validateFileContextFreshnessBeforeEdit(toolCall);
          if (staleError) {
            return {
              toolCallId: toolCall.id,
              content: staleError,
              isError: true,
            };
          }
        }
        const result = await this.fileExecutor.execute(toolCall);
        this.recordFileContextAfterFileTool(toolCall, result);
        return result;
      }

      case 'grep_search':
        return await this.searchExecutor.execute(toolCall);

      case 'file_remove':
        return await this.removeFilesExecutor.execute(toolCall);

      case 'web_search':
      case 'web_fetch':
        return await this.webExecutor.execute(toolCall, signal);

      case 'mermaid_diagram':
        return await this.mermaidExecutor.execute(toolCall);

      case 'codebase_retrieval':
        return await this.aceExecutor.execute(toolCall, signal);

      case 'dispatch_task':
      case 'send_worker_message':
      case 'wait_for_workers':
        return await this.orchestrationExecutor.execute(toolCall);

      case 'get_todos':
      case 'update_todo':
      case 'split_todo': {
        // 编排层 todo 工具需要调用方上下文（标识当前 worker/assignment/todo）
        const execCtx = this.executionContextStorage.getStore();
        const callerContext = execCtx?.workerId
          ? this.snapshotContextMap.get(execCtx.workerId)
          : undefined;
        return await this.orchestrationExecutor.execute(toolCall, callerContext);
      }

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

    // 2. 仅高风险工具需要用户授权（只读工具不需要）
    const requiresAuthorization = this.requiresUserAuthorization(toolCall.name);
    if (!requiresAuthorization) {
      return { allowed: true };
    }

    // 3. 高风险工具必须有授权回调
    if (!this.authorizationCallback) {
      return { allowed: false, reason: 'Tool authorization handler not configured' };
    }

    // 4. 请求用户授权（Ask 模式下会弹窗，Auto 模式由回调直接放行）
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
    // 有副作用的终端工具需要 allowBash 权限
    // 注意：read-process / list-processes 是只读操作，不需要 allowBash
    if (
      toolName === 'launch-process'
      || toolName === 'write-process'
      || toolName === 'kill-process'
    ) {
      if (!this.permissions.allowBash) {
        return { allowed: false, reason: 'Terminal command execution is disabled' };
      }
      return { allowed: true };
    }

    // 文件写入工具需要 allowEdit 权限
    if (
      toolName === 'file_create'
      || toolName === 'file_edit'
      || toolName === 'file_insert'
      || toolName === 'file_bulk_edit'
      || toolName === 'file_remove'
    ) {
      if (!this.permissions.allowEdit) {
        return { allowed: false, reason: 'File editing is disabled' };
      }
      return { allowed: true };
    }

    // Web 工具需要 allowWeb 权限（精确匹配，避免误拦名字含 'web' 的 MCP 工具）
    if (toolName === 'web_search' || toolName === 'web_fetch') {
      if (!this.permissions.allowWeb) {
        return { allowed: false, reason: 'Web access is disabled' };
      }
      return { allowed: true };
    }

    // 其他工具默认允许（只读工具、编排工具、MCP 工具等）
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
   * 生成可用工具的自然语言摘要（单一 source of truth）
   *
   * Orchestrator 和 Worker 共用此方法获取工具信息，
   * 消除多处硬编码工具列表的维护负担。
   *
   * @param options.role 调用角色：'orchestrator' 包含编排者专用说明，'worker' 省略
   * @param options.excludeOrchestrationTools 是否排除编排工具（Worker 默认排除）
   */
  async buildToolsSummary(options?: {
    role?: 'orchestrator' | 'worker';
    excludeOrchestrationTools?: boolean;
  }): Promise<string> {
    const role = options?.role ?? 'worker';
    const excludeOrch = options?.excludeOrchestrationTools ?? true;

    const tools = await this.getTools();
    if (tools.length === 0) {
      return '';
    }

    const orchestrationToolNames = ['dispatch_task', 'send_worker_message', 'wait_for_workers'];
    const workerOnlyToolNames = ['split_todo'];
    const hideOrchestrationTools = role === 'worker' && excludeOrch;
    const hideWorkerOnlyTools = role === 'orchestrator';
    const builtinTools = tools.filter(t =>
      t.metadata?.source === 'builtin' &&
      (!hideOrchestrationTools || !orchestrationToolNames.includes(t.name)) &&
      (!hideWorkerOnlyTools || !workerOnlyToolNames.includes(t.name))
    );
    const visibleBuiltinToolNames = new Set(builtinTools.map(t => t.name));

    // 内置工具描述映射（中文用途说明）
    const builtinToolDescriptions: Record<string, { category: string; desc: string }> = {
      'file_view': { category: '文件操作', desc: '查看文件内容或浏览目录结构' },
      'file_create': { category: '文件操作', desc: '创建新文件或写入完整文件内容' },
      'file_edit': { category: '文件操作', desc: '精确替换文件中的文本' },
      'file_insert': { category: '文件操作', desc: '在指定行插入文本' },
      'file_remove': { category: '文件操作', desc: '删除文件' },
      'grep_search': { category: '文件操作', desc: '正则搜索代码内容' },
      'launch-process': { category: '终端命令', desc: '执行构建/测试/启动服务等进程，也可用于 sed/python/node 脚本批量编辑文件（单文件精确编辑优先用 file_edit）' },
      'read-process': { category: '终端命令', desc: '读取终端进程输出' },
      'write-process': { category: '终端命令', desc: '向运行中的终端写入输入' },
      'kill-process': { category: '终端命令', desc: '终止终端进程' },
      'list-processes': { category: '终端命令', desc: '列出所有终端进程' },
      'web_search': { category: '网络工具', desc: '搜索互联网信息' },
      'web_fetch': { category: '网络工具', desc: '获取 URL 页面内容' },
      'codebase_retrieval': { category: '代码智能', desc: '语义搜索代码库' },
      'mermaid_diagram': { category: '可视化', desc: '生成 Mermaid 图表' },
      'split_todo': { category: '任务管理', desc: '将当前任务拆分为多个子步骤' },
      'get_todos': { category: '任务管理', desc: '查看当前任务的 Todo 列表' },
      'update_todo': { category: '任务管理', desc: '更新 Todo 状态或内容' },
    };

    // 编排者专用的附加说明
    const orchestratorNotes: Record<string, string> = {
      'file_edit': '（编排者限改 3 个文件内的简单修改，复杂修改委派 Worker）',
      'file_create': '（编排者限 3 个文件内）',
      'file_insert': '（编排者限 3 个文件内）',
      'file_remove': '（编排者限 3 个文件内）',
    };

    const lines: string[] = [];

    // 内置工具：按类别分组
    lines.push('内置工具:');
    const categoryOrder = ['文件操作', '终端命令', '网络工具', '代码智能', '可视化', '任务管理'];
    for (const category of categoryOrder) {
      const categoryTools = Object.entries(builtinToolDescriptions)
        .filter(([name, v]) => v.category === category && visibleBuiltinToolNames.has(name));
      if (categoryTools.length > 0) {
        const toolList = categoryTools.map(([name, v]) => {
          const note = role === 'orchestrator' ? (orchestratorNotes[name] || '') : '';
          return `${name}（${v.desc}${note}）`;
        }).join('、');
        lines.push(`- ${category}：${toolList}`);
      }
    }

    // 动态发现新增的未映射内置工具
    const unmappedTools = builtinTools.filter(t => !builtinToolDescriptions[t.name]);
    for (const tool of unmappedTools) {
      const desc = tool.description ? tool.description.split(/[。\n]/)[0].substring(0, 60) : '';
      lines.push(`- 其他：${tool.name}（${desc}）`);
    }

    // MCP 工具
    const mcpTools = tools.filter(t => t.metadata?.source === 'mcp');
    if (mcpTools.length > 0) {
      lines.push('');
      lines.push('MCP 扩展工具（用户已安装，可直接调用）:');
      for (const tool of mcpTools) {
        const desc = tool.description ? ` - ${tool.description.substring(0, 80)}` : '';
        lines.push(`- ${tool.name}${desc}`);
      }
    }

    // Skill 自定义工具
    const skillTools = tools.filter(t => t.metadata?.source === 'skill');
    if (skillTools.length > 0) {
      lines.push('');
      lines.push('Skill 自定义工具（用户已安装，可直接调用）:');
      for (const tool of skillTools) {
        const desc = tool.description ? ` - ${tool.description.substring(0, 80)}` : '';
        lines.push(`- ${tool.name}${desc}`);
      }
    }

    return lines.join('\n');
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
        description: `Launch a shell command in an agent-dedicated terminal. Terminal identity is auto-injected by the system.

Process modes:
- run_mode="task": one-shot command, terminal becomes reusable after completion.
- run_mode="service": long-running background service, terminal is locked until kill-process.
- run_mode="service" + wait=true: wait only for startup handshake (not process exit), then return running/ready state.
- wait timeout or user interrupt only stops waiting; process keeps running unless kill-process is called.

If run_mode is omitted, system defaults to:
- wait=true => task
- wait=false => service
Additionally, long-running commands like dev/start/serve/watch may be auto-inferred as service to avoid accidental timeout-kill.

Tool selection guidance:
- Single-file precise edits: prefer file_edit/file_insert (structured, reliable line anchoring)
- Batch/repetitive multi-file changes: use launch-process with sed/python/node scripts (more efficient)
- Full-file creation/overwrite: prefer file_create
- Read files or browse directories: use file_view, NOT cat/head/tail
- Search code content: use grep_search, NOT grep/rg
- Search the web: use web_search, NOT curl
- Fetch a URL: use web_fetch, NOT curl/wget

Use launch-process for tasks that truly benefit from shell execution: build, test, lint, git, package manager, start server, database migration, or scripted bulk edits.

Only set may_modify_files=true when the command directly modifies source files (sed -i, python scripts writing files, echo > file, etc.).
Do NOT set it for read-only commands such as ls/cat/grep/git status/log/diff.
After a mutating command, refresh each target file with file_view once before file_edit/file_insert (no need to repeatedly view the same file).`,
        input_schema: {
          type: 'object',
          properties: {
            command: { type: 'string', description: '要执行的 shell 命令（不要在 command 中写 cd，目录请通过 cwd 传递）' },
            cwd: { type: 'string', description: '命令执行目录。单工作区可省略（自动使用工作区根目录）；多工作区必须显式指定 "<工作区名>" 或 "<工作区名>/相对路径"' },
            wait: { type: 'boolean', description: '是否等待进程完成（默认 true）' },
            run_mode: { type: 'string', description: '运行模式：task（一次性命令）或 service（长驻服务）。默认规则：wait=true -> task，wait=false -> service；对 dev/start/serve/watch 等长驻命令会自动推断为 service。', enum: ['task', 'service'] },
            max_wait_seconds: { type: 'number', description: '空闲超时秒数（距最近一次输出超过该值则判定超时，默认 30）' },
            startup_wait_seconds: { type: 'number', description: '仅 service 模式生效。wait=true 时用于启动握手等待秒数（默认 5）。' },
            ready_patterns: {
              type: 'array',
              description: '仅 service 模式生效。可选的就绪日志正则字符串数组，命中后 phase 进入 ready。',
              items: { type: 'string' },
            },
            showTerminal: { type: 'boolean', description: '是否显示终端窗口（默认 true）' },
            may_modify_files: { type: 'boolean', description: '命令是否会直接修改后续要编辑的源文件。仅在确实会改文件时设为 true；ls/cat/grep/git status 等只读命令应保持 false。默认 false（系统也会做有限启发式检测）。' },
          },
          required: ['command'],
        },
      },
      {
        name: 'read-process',
        description: `Read terminal process output and status. Safe for long-running services — timeout only stops waiting, never kills the process.

Usage patterns:
- wait=false (default): return current output immediately, no blocking.
- wait=true: block until process completes or idle timeout is reached. If the process is still running when timeout fires, returns status="running" with current output — the process keeps running.
- from_cursor: optional incremental read cursor. Use previous next_cursor to fetch only newly appended output.

Typical workflow for background services:
  1. launch-process("npm run dev", run_mode="service", wait=false) → terminal_id=1
  2. read-process(terminal_id=1, wait=true, max_wait_seconds=5) → check startup logs
  3. read-process(terminal_id=1, from_cursor=<next_cursor>) → read incremental logs.`,
        input_schema: {
          type: 'object',
          properties: {
            terminal_id: { type: 'number', description: 'terminal_id（来自 launch-process）' },
            wait: { type: 'boolean', description: '是否等待状态变化（默认 false）' },
            max_wait_seconds: { type: 'number', description: '空闲超时秒数（距最近一次输出超过该值则判定超时，默认 30）' },
            from_cursor: { type: 'number', description: '增量读取起始游标。传上一次返回的 next_cursor 只读取新增输出。' },
          },
          required: ['terminal_id'],
        },
      },
      {
        name: 'write-process',
        description: `Write text to a running terminal process stdin. After writing, use read-process to see the response.
Only works when the process is in "running" state; returns accepted=false otherwise.`,
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
        description: 'Terminate a terminal process by sending SIGINT and disposing the terminal. For service mode this also releases terminal lock.',
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
        description: 'List all terminal process records with their current status, command, working directory, and elapsed time. Use this to check which processes are still running.',
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

    // 6. 文件操作工具 (file_view, file_create, file_edit, file_insert)
    tools.push(...this.fileExecutor.getToolDefinitions());

    // 7. grep_search (代码搜索)
    tools.push(this.searchExecutor.getToolDefinition());

    // 8. file_remove (文件删除)
    tools.push(this.removeFilesExecutor.getToolDefinition());

    // 9-10. web_search, web_fetch (网络搜索/获取)
    tools.push(...this.webExecutor.getToolDefinitions());

    // 11. mermaid_diagram (Mermaid 图表)
    tools.push(this.mermaidExecutor.getToolDefinition());

    // 12. codebase_retrieval (ACE 语义搜索)
    tools.push(this.aceExecutor.getToolDefinition());

    // 13-15. 编排工具 (dispatch_task, send_worker_message, wait_for_workers)
    tools.push(...this.orchestrationExecutor.getToolDefinitions());

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

  private normalizeLaunchProcessCommand(rawCommand: unknown, rawCwd: unknown): {
    command: string | null;
    cwd: string;
    error?: string;
  } {
    if (typeof rawCommand !== 'string' || !rawCommand.trim()) {
      return {
        command: null,
        cwd: '',
        error: 'command 参数必须是非空字符串',
      };
    }

    let command = rawCommand.trim();
    let cwd = typeof rawCwd === 'string' ? rawCwd.trim() : '';

    // 允许 heredoc（<<EOF），但必须在命令字符串中闭合终止符，避免终端进入持续等待输入状态。
    const heredocCheck = this.validateHeredocCommand(command);
    if (!heredocCheck.valid) {
      return {
        command: null,
        cwd: '',
        error: heredocCheck.error || 'heredoc 语法校验失败',
      };
    }

    const inlineCdPattern = /^\s*cd\s+(?:"([^"]+)"|'([^']+)'|([^;&|]+?))\s*(?:&&|;)\s*(.+)$/s;
    const inlineCdMatch = command.match(inlineCdPattern);
    if (inlineCdMatch) {
      const inlineCwd = (inlineCdMatch[1] || inlineCdMatch[2] || inlineCdMatch[3] || '').trim();
      const nextCommand = (inlineCdMatch[4] || '').trim();

      if (!nextCommand) {
        return {
          command: null,
          cwd: '',
          error: 'command 中的 cd 后缺少实际可执行命令',
        };
      }

      if (cwd && cwd !== inlineCwd) {
        return {
          command: null,
          cwd: '',
          error: `cwd 参数与 command 内联 cd 冲突（cwd="${cwd}" vs cd "${inlineCwd}"）`,
        };
      }

      cwd = inlineCwd;
      command = nextCommand;
    } else if (/^\s*cd\s+/.test(command)) {
      return {
        command: null,
        cwd: '',
        error: '不要在 command 中单独执行 cd；请把目录写入 cwd，command 仅保留实际命令',
      };
    }

    return { command, cwd };
  }

  private validateHeredocCommand(command: string): { valid: true } | { valid: false; error: string } {
    // 匹配 heredoc 起始（支持 <<EOF / <<'EOF' / <<"EOF" / <<-EOF）
    const heredocRegex = /<<(-)?\s*(?:(['"])([^'"\s]+)\2|([^\s<>&|;]+))/g;
    let match: RegExpExecArray | null;

    while ((match = heredocRegex.exec(command)) !== null) {
      const allowTabIndent = match[1] === '-';
      const delimiter = (match[3] || match[4] || '').trim();
      if (!delimiter) {
        continue;
      }

      const startIndex = match.index + match[0].length;
      const firstNewlineIndex = command.indexOf('\n', startIndex);
      if (firstNewlineIndex < 0) {
        return {
          valid: false,
          error: `heredoc 缺少结束标记（${delimiter}）。请确保命令包含独立一行的结束符`,
        };
      }

      const payload = command.slice(firstNewlineIndex + 1);
      const escapedDelimiter = delimiter.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const terminatorRegex = allowTabIndent
        ? new RegExp(`^(?:\\t*)${escapedDelimiter}[ \\t]*$`, 'm')
        : new RegExp(`^${escapedDelimiter}[ \\t]*$`, 'm');

      if (!terminatorRegex.test(payload)) {
        return {
          valid: false,
          error: `heredoc 未闭合（结束标记 ${delimiter} 未找到）。请补全结束符，或改用 file_create/file_edit/file_insert`,
        };
      }
    }

    return { valid: true };
  }

  private resolveLaunchProcessWorkspacePath(
    inputPath: string,
    preferWorkspacePath?: string
  ): { absolutePath: string | null; error?: string } {
    try {
      const resolved = this.workspaceRoots.resolvePath(inputPath, {
        mustExist: true,
        preferWorkspacePath,
      });
      if (!resolved?.absolutePath) {
        return {
          absolutePath: null,
          error: `cwd "${inputPath}" 不存在，或不在当前工作区内`,
        };
      }
      return { absolutePath: resolved.absolutePath };
    } catch (error: any) {
      return {
        absolutePath: null,
        error: `cwd "${inputPath}" 解析失败: ${error?.message || String(error)}`,
      };
    }
  }

  private resolveLaunchProcessCwd(rawCwd: unknown): { absolutePath: string | null; error?: string } {
    if (rawCwd !== undefined && rawCwd !== null && typeof rawCwd !== 'string') {
      return {
        absolutePath: null,
        error: 'cwd 参数类型错误，必须是字符串',
      };
    }

    const hasMultipleRoots = this.workspaceRoots.hasMultipleRoots();
    const workspaceFolders = this.workspaceRoots.getFolders();
    const workspaceNames = workspaceFolders.map(folder => folder.name);
    const requestedCwd = typeof rawCwd === 'string' ? rawCwd.trim() : '';
    const workspaceHint = hasMultipleRoots
      ? `可用工作区: ${workspaceNames.join('、')}。请使用 "<工作区名>" 或 "<工作区名>/相对路径"，不要使用固定系统路径（如 /home/user）。`
      : '请使用当前工作区内路径。';

    if (!requestedCwd) {
      if (hasMultipleRoots) {
        return {
          absolutePath: null,
          error: `多工作区必须显式指定 cwd。${workspaceHint}`,
        };
      }
      return this.resolveLaunchProcessWorkspacePath('.', this.workspaceRoot);
    }

    if (path.isAbsolute(requestedCwd)) {
      const absolute = this.resolveLaunchProcessWorkspacePath(requestedCwd);
      if (absolute.absolutePath) {
        return absolute;
      }
      return {
        absolutePath: null,
        error: `cwd "${requestedCwd}" 不在当前工作区内，或目录不存在。${workspaceHint}`,
      };
    }

    const matchedWorkspace = workspaceFolders.find(folder => folder.name === requestedCwd);
    if (matchedWorkspace) {
      return { absolutePath: matchedWorkspace.path };
    }

    if (hasMultipleRoots) {
      const slash = requestedCwd.indexOf('/');
      const prefix = slash > 0 ? requestedCwd.substring(0, slash) : '';
      const hasWorkspacePrefix = workspaceNames.includes(prefix);

      if (!hasWorkspacePrefix) {
        return {
          absolutePath: null,
          error: `多工作区 cwd 必须显式包含工作区名。${workspaceHint}`,
        };
      }
    }

    return this.resolveLaunchProcessWorkspacePath(requestedCwd, this.workspaceRoot);
  }

  private resolveLaunchProcessTerminalName(context?: SnapshotContext): { terminalName: string | null; error?: string } {
    const workerId = context?.workerId || this.getExecutionWorkerId() || 'orchestrator';

    if (workerId === 'orchestrator') {
      return { terminalName: 'orchestrator' };
    }

    const workerSlotSet = new Set(['claude', 'gemini', 'codex']);
    if (workerSlotSet.has(workerId)) {
      return { terminalName: `worker-${workerId}` };
    }

    const agentTerminalSet = new Set(['worker-claude', 'worker-gemini', 'worker-codex']);
    if (agentTerminalSet.has(workerId)) {
      return { terminalName: workerId };
    }

    return {
      terminalName: null,
      error: `未知 workerId "${workerId}"，仅支持 orchestrator、claude、gemini、codex`,
    };
  }

  /**
   * 自动识别常见长驻命令，避免在未显式指定 run_mode 时误判为 task 后被超时终止。
   */
  private isLikelyLongRunningServiceCommand(command: string): boolean {
    if (!command || typeof command !== 'string') {
      return false;
    }
    const normalized = command.trim().toLowerCase();
    if (!normalized) {
      return false;
    }

    const servicePatterns: RegExp[] = [
      /\b(npm|pnpm|yarn|bun)\s+(run\s+)?(dev|start|serve|preview|watch)\b/i,
      /\b(vite|webpack|next|nuxt|astro)\b[^\n]*\b(dev|start|serve|watch)\b/i,
      /\b(node|tsx|ts-node|nodemon)\b[^\n]*\b(server|dev|watch)\b/i,
      /\bpython(?:3)?\s+-m\s+http\.server\b/i,
      /\b(uvicorn|gunicorn)\b/i,
      /\bdocker\s+compose\s+up\b/i,
      /\bkubectl\s+port-forward\b/i,
      /\btail\s+-f\b/i,
    ];

    return servicePatterns.some((pattern) => pattern.test(normalized));
  }

  private isLikelyFileMutatingCommand(command: string): boolean {
    const patterns = [
      /\bsed\s+-i(?:\S*)?\b/i,
      /\bperl\b[^\n]*\s-i(?:\S*)?\b/i,
      /\b(?:g?awk)\b[^\n]*\s-i\s+inplace\b/i,
      /\bpython(?:3)?\b[^\n]*\s+[^;&|]*\.py\b/i,
      /\bnode\b[^\n]*\s+[^;&|]*\.(?:[cm]?js|ts)\b/i,
      /\b(?:bash|sh|zsh)\b[^\n]*\s+[^;&|]*\.(?:sh|bash|zsh)\b/i,
      /\bpython(?:3)?\b[^\n]*\s-c\b/i,
      /\bpython(?:3)?\b[^\n]*<<-?\s*['"]?[A-Za-z_][A-Za-z0-9_]*['"]?/i,
      /\becho\b[\s\S]*?(?:>>|>)\s*(?!\/dev\/null\b)\S+/i,
      /\bcat\b[\s\S]*?(?:>>|>)\s*(?!\/dev\/null\b)\S+/i,
      /\btee\b(?:\s+-a)?\s+(?!\/dev\/null\b)\S+/i,
      /\b(prettier|eslint|ruff)\b[^\n]*\b(--write|--fix)\b/i,
      /\bgit\s+(apply|am|checkout|restore|reset|merge|cherry-pick|rebase|pull)\b/i,
    ];
    return patterns.some((pattern) => pattern.test(command));
  }

  /**
   * 兜底检测：识别“执行脚本型命令”。
   * 这类命令即使未命中更精确的写入特征，也常常会直接修改代码文件。
   */
  private isLikelyScriptDrivenMutation(command: string): boolean {
    const patterns = [
      /\bpython(?:3)?\b(?![^\n]*\b-m\s+(?:pytest|unittest|pip|http\.server)\b)[^\n]*\s+(?!-)(?:[^;&|\s]*[/.][^;&|\s]*)(?:\s|$)/i,
      /\bnode\b[^\n]*\s+(?!-)(?:[^;&|\s]*[/.][^;&|\s]*)(?:\s|$)/i,
      /\b(?:bash|sh|zsh)\b[^\n]*\s+(?!-)(?:[^;&|\s]*[/.][^;&|\s]*)(?:\s|$)/i,
      /\bnpx\b[^\n]*\b(jscodeshift|codemod|ts-node|tsx)\b/i,
    ];
    return patterns.some((pattern) => pattern.test(command));
  }

  private isLikelyReadOnlyCommand(command: string): boolean {
    const segments = command
      .split(/&&|\|\||;/)
      .map(segment => segment.trim())
      .filter(Boolean);
    if (segments.length === 0) {
      return false;
    }

    const readOnlyPatterns = [
      /^(?:env\s+)?(?:ls|pwd|cat|head|tail|wc|grep|rg|find|tree|stat|file|which|whereis|du|df)\b/i,
      /^echo\b(?![\s\S]*?(?:>>|>)\s*(?!\/dev\/null\b)\S+)/i,
      /^git\s+(status|log|show|diff|branch|rev-parse|remote|tag)\b/i,
      /^(?:npm|pnpm|yarn)\s+(list|why|info|outdated)\b/i,
    ];

    return segments.every((segment) => {
      const normalizedSegment = segment.replace(/\d?>\s*\/dev\/null\b/g, '').trim();
      if (!normalizedSegment) {
        return true;
      }
      if (this.isLikelyFileMutatingCommand(normalizedSegment)) {
        return false;
      }
      return readOnlyPatterns.some((pattern) => pattern.test(normalizedSegment));
    });
  }

  private markWorkspacePossiblyMutated(source: string, command: string): void {
    this.workspaceMutationEpoch += 1;
    logger.info('Workspace mutation epoch advanced', {
      source,
      workspaceMutationEpoch: this.workspaceMutationEpoch,
      command,
    }, LogCategory.TOOLS);
  }

  private resolveFileToolPath(toolCall: ToolCall, mustExist: boolean): string | null {
    const args = toolCall.arguments as any;
    const rawPath = typeof args?.path === 'string' ? args.path : '';
    if (!rawPath) {
      return null;
    }
    try {
      const resolved = this.workspaceRoots.resolvePath(rawPath, { mustExist });
      return resolved?.absolutePath || null;
    } catch {
      return null;
    }
  }

  /**
   * 上下文新鲜度校验：
   * 当工作区可能被外部命令改写后，file_edit/file_insert 必须先对目标文件执行一次 file_view。
   * 这样可保证后续行锚点与 old_str 基于最新文件状态，避免“调用成功但无改动”。
   */
  private validateFileContextFreshnessBeforeEdit(toolCall: ToolCall): string | null {
    // file_insert 允许创建新文件；新文件不存在时无需 freshness 校验。
    const absPath = this.resolveFileToolPath(toolCall, true);
    if (!absPath) {
      return null;
    }

    if (this.workspaceMutationEpoch === 0) {
      return null;
    }

    const fileEpoch = this.fileContextEpochByPath.get(absPath);
    if (fileEpoch !== undefined && fileEpoch >= this.workspaceMutationEpoch) {
      return null;
    }

    const displayPath = this.workspaceRoots.toDisplayPath(absPath);
    return `Error [FILE_CONTEXT_STALE]: ${displayPath} may have changed after external mutations. Run file_view on this file once, then retry file_edit/file_insert with refreshed anchors.`;
  }

  private recordFileContextAfterFileTool(toolCall: ToolCall, result: ToolResult): void {
    if (result.isError) {
      return;
    }

    const args = toolCall.arguments as any;

    if (toolCall.name === 'file_view') {
      if (args?.type === 'directory') {
        return;
      }
      const absPath = this.resolveFileToolPath(toolCall, true);
      if (!absPath) {
        return;
      }
      try {
        const stat = fs.statSync(absPath);
        if (!stat.isFile()) {
          return;
        }
      } catch {
        return;
      }
      this.fileContextEpochByPath.set(absPath, this.workspaceMutationEpoch);
      return;
    }

    if (toolCall.name === 'file_edit' || toolCall.name === 'file_insert' || toolCall.name === 'file_create') {
      const absPath = this.resolveFileToolPath(toolCall, false);
      if (absPath) {
        this.fileContextEpochByPath.set(absPath, this.workspaceMutationEpoch);
      }
    }
  }

  private async executeLaunchProcessTool(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const {
      command: rawCommand,
      cwd: rawCwd,
      wait = true,
      run_mode: rawRunMode,
      max_wait_seconds = 30,
      startup_wait_seconds,
      ready_patterns,
      showTerminal = true,
      may_modify_files = false,
    } = args;
    const normalized = this.normalizeLaunchProcessCommand(rawCommand, rawCwd);
    if (!normalized.command) {
      return {
        toolCallId: toolCall.id,
        content: `Command rejected: ${normalized.error || 'command 参数错误'}`,
        isError: true,
      };
    }
    if (args.wait !== undefined && typeof args.wait !== 'boolean') {
      return {
        toolCallId: toolCall.id,
        content: 'Command rejected: wait 参数类型错误，必须是 boolean',
        isError: true,
      };
    }
    if (rawRunMode !== undefined && rawRunMode !== 'task' && rawRunMode !== 'service') {
      return {
        toolCallId: toolCall.id,
        content: 'Command rejected: run_mode 参数错误，只能是 "task" 或 "service"',
        isError: true,
      };
    }
    if (args.max_wait_seconds !== undefined && (typeof args.max_wait_seconds !== 'number' || !Number.isFinite(args.max_wait_seconds))) {
      return {
        toolCallId: toolCall.id,
        content: 'Command rejected: max_wait_seconds 参数类型错误，必须是 number',
        isError: true,
      };
    }
    if (startup_wait_seconds !== undefined && (typeof startup_wait_seconds !== 'number' || !Number.isFinite(startup_wait_seconds) || startup_wait_seconds < 0)) {
      return {
        toolCallId: toolCall.id,
        content: 'Command rejected: startup_wait_seconds 参数类型错误，必须是 >= 0 的 number',
        isError: true,
      };
    }
    if (
      ready_patterns !== undefined
      && (!Array.isArray(ready_patterns) || ready_patterns.some((item: unknown) => typeof item !== 'string'))
    ) {
      return {
        toolCallId: toolCall.id,
        content: 'Command rejected: ready_patterns 参数类型错误，必须是 string[]',
        isError: true,
      };
    }
    if (args.showTerminal !== undefined && typeof args.showTerminal !== 'boolean') {
      return {
        toolCallId: toolCall.id,
        content: 'Command rejected: showTerminal 参数类型错误，必须是 boolean',
        isError: true,
      };
    }
    if (args.may_modify_files !== undefined && typeof args.may_modify_files !== 'boolean') {
      return {
        toolCallId: toolCall.id,
        content: 'Command rejected: may_modify_files 参数类型错误，必须是 boolean',
        isError: true,
      };
    }

    // 终端名称由系统根据执行上下文自动推导，不再依赖 LLM 传入
    const activeContext = this.getActiveSnapshotContext();
    const terminalResolution = this.resolveLaunchProcessTerminalName(activeContext);
    if (!terminalResolution.terminalName) {
      return {
        toolCallId: toolCall.id,
        content: `Command rejected: ${terminalResolution.error}`,
        isError: true,
      };
    }
    const terminalName = terminalResolution.terminalName;

    const validation = this.terminalExecutor.validateCommand(normalized.command);
    if (!validation.valid) {
      return {
        toolCallId: toolCall.id,
        content: `Command rejected: ${validation.reason}`,
        isError: true,
      };
    }

    const cwdResolution = this.resolveLaunchProcessCwd(normalized.cwd);
    if (!cwdResolution.absolutePath) {
      return {
        toolCallId: toolCall.id,
        content: `Command rejected: ${cwdResolution.error}`,
        isError: true,
      };
    }

    const resolvedCwd = cwdResolution.absolutePath;
    logger.debug('launch-process cwd resolved', {
      rawCwd,
      normalizedCwd: normalized.cwd,
      resolvedCwd,
      normalizedCommand: normalized.command,
      terminalName,
    }, LogCategory.TOOLS);

    const inferredServiceMode = rawRunMode === undefined
      && wait === true
      && this.isLikelyLongRunningServiceCommand(normalized.command);
    const runMode: ProcessRunMode = rawRunMode === 'task' || rawRunMode === 'service'
      ? rawRunMode
      : (wait && !inferredServiceMode ? 'task' : 'service');
    if (inferredServiceMode) {
      logger.info('launch-process 自动推断为 service 模式（长驻命令）', {
        command: normalized.command,
      }, LogCategory.TOOLS);
    }
    const effectiveWait = wait;
    const result = await this.terminalExecutor.launchProcess({
      command: normalized.command,
      cwd: resolvedCwd,
      wait: effectiveWait,
      maxWaitSeconds: max_wait_seconds,
      name: terminalName,
      showTerminal,
      runMode,
      startupWaitSeconds: startup_wait_seconds,
      readyPatterns: ready_patterns,
    }, signal);

    const hasFailureStatus = result.status === 'failed' || result.status === 'killed' || result.status === 'timeout';
    const hasNonZeroExit = result.return_code !== null && result.return_code !== 0;
    const isError = hasFailureStatus || hasNonZeroExit;
    const explicitMayModify = may_modify_files === true;
    const heuristicMayModify = this.isLikelyFileMutatingCommand(normalized.command);
    const scriptDrivenMayModify = this.isLikelyScriptDrivenMutation(normalized.command);
    const readOnlyCommand = this.isLikelyReadOnlyCommand(normalized.command);
    const likelyMutatesFiles = !readOnlyCommand && (explicitMayModify || heuristicMayModify || scriptDrivenMayModify);
    if (likelyMutatesFiles) {
      this.markWorkspacePossiblyMutated('launch-process', normalized.command);
      if (scriptDrivenMayModify && !explicitMayModify && !heuristicMayModify) {
        logger.info('launch-process script-driven mutation detected by fallback heuristic', {
          command: normalized.command,
        }, LogCategory.TOOLS);
      }
    } else if (explicitMayModify && readOnlyCommand) {
      logger.info('launch-process may_modify_files=true ignored for read-only command', {
        command: normalized.command,
      }, LogCategory.TOOLS);
    }

    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError,
    };
  }

  private async executeReadProcessTool(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const { terminal_id, wait = false, max_wait_seconds = 30, from_cursor } = args;

    if (terminal_id === undefined || typeof terminal_id !== 'number') {
      return {
        toolCallId: toolCall.id,
        content: 'read-process rejected: terminal_id 参数缺失或类型错误，必须是 number',
        isError: true,
      };
    }
    if (args.wait !== undefined && typeof args.wait !== 'boolean') {
      return {
        toolCallId: toolCall.id,
        content: 'read-process rejected: wait 参数类型错误，必须是 boolean',
        isError: true,
      };
    }
    if (
      args.max_wait_seconds !== undefined
      && (typeof args.max_wait_seconds !== 'number' || !Number.isFinite(args.max_wait_seconds))
    ) {
      return {
        toolCallId: toolCall.id,
        content: 'read-process rejected: max_wait_seconds 参数类型错误，必须是 number',
        isError: true,
      };
    }
    if (from_cursor !== undefined && (!Number.isInteger(from_cursor) || from_cursor < 0)) {
      return {
        toolCallId: toolCall.id,
        content: 'read-process rejected: from_cursor 参数类型错误，必须是 >= 0 的整数',
        isError: true,
      };
    }

    const result = await this.terminalExecutor.readProcess(terminal_id, wait, max_wait_seconds, from_cursor, signal);
    const hasFailureStatus = result.status === 'failed' || result.status === 'killed' || result.status === 'timeout';
    const hasNonZeroExit = result.return_code !== null && result.return_code !== 0;
    const isError = hasFailureStatus || hasNonZeroExit;
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError,
    };
  }

  private async executeWriteProcessTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const { terminal_id, input_text } = args;

    if (terminal_id === undefined || typeof terminal_id !== 'number') {
      return {
        toolCallId: toolCall.id,
        content: 'write-process rejected: terminal_id 参数缺失或类型错误，必须是 number',
        isError: true,
      };
    }
    if (input_text === undefined || typeof input_text !== 'string') {
      return {
        toolCallId: toolCall.id,
        content: 'write-process rejected: input_text 参数缺失或类型错误，必须是 string',
        isError: true,
      };
    }

    const result = await this.terminalExecutor.writeProcess(terminal_id, input_text);
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError: !result.accepted,
    };
  }

  private async executeKillProcessTool(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as any;
    const { terminal_id } = args;

    if (terminal_id === undefined || typeof terminal_id !== 'number') {
      return {
        toolCallId: toolCall.id,
        content: 'kill-process rejected: terminal_id 参数缺失或类型错误，必须是 number',
        isError: true,
      };
    }

    const result = await this.terminalExecutor.killProcess(terminal_id);
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(result),
      isError: !result.killed,
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
    toolDef: ExtendedToolDefinition,
    signal?: AbortSignal
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

    return await executor.execute(toolCall, signal);
  }

  /**
   * 使缓存失效
   */
  private invalidateCache(): void {
    this.toolCache.clear();
  }

  /**
   * 主动刷新工具 schema 缓存
   *
   * 用于运行时配置变更（如 Worker 分工/启用状态变化）后，
   * 确保下一次工具拉取使用最新定义，避免 enum 陈旧。
   */
  refreshToolSchemas(): void {
    this.invalidateCache();
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
   * 获取工作区目录列表
   */
  getWorkspaceFolders(): WorkspaceFolderInfo[] {
    return this.workspaceRoots.getFolders();
  }

  /**
   * 获取工作区展示文本（用于系统提示）
   */
  getWorkspacePromptDisplay(): string {
    const folders = this.workspaceRoots.getFolders();
    if (folders.length === 1) {
      return folders[0].path;
    }

    const lines = folders.map(folder => `  - ${folder.name}: ${folder.path}`);
    return `多工作区:\n${lines.join('\n')}`;
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
