/**
 * 工具系统类型定义
 */

import { ToolDefinition, ToolCall, ToolResult } from '../llm/types';

// ============================================================================
// 工具源类型
// ============================================================================

/**
 * 工具来源
 */
export type ToolSource = 'mcp' | 'skill' | 'builtin';

/**
 * 系统内置工具名称（单一真相来源）
 *
 * 所有需要判断"工具是否为内置"的模块（ToolManager、Adapter 等）
 * 必须引用此常量，禁止各自硬编码。
 */
export const BUILTIN_TOOL_NAMES = [
  'launch-process',
  'read-process',
  'write-process',
  'kill-process',
  'list-processes',
  'file_view',
  'file_create',
  'file_edit',
  'file_insert',
  'file_bulk_edit',
  'file_remove',
  'grep_search',
  'web_search',
  'web_fetch',
  'mermaid_diagram',
  'codebase_retrieval',
  'dispatch_task',
  'send_worker_message',
  'wait_for_workers',
  'split_todo',
  'get_todos',
  'update_todo',
] as const;

/** 内置工具名称的联合类型 */
export type BuiltinToolName = typeof BUILTIN_TOOL_NAMES[number];

/**
 * 工具元数据
 */
export interface ToolMetadata {
  source: ToolSource;
  sourceId?: string; // MCP server ID 或 Skill ID
  category?: string;
  tags?: string[];
}

/**
 * 扩展的工具定义（包含元数据）
 */
export interface ExtendedToolDefinition extends ToolDefinition {
  metadata: ToolMetadata;
}

// ============================================================================
// 工具执行器接口
// ============================================================================

/**
 * 工具执行器接口
 */
export interface ToolExecutor {
  /**
   * 执行工具调用
   * @param signal 可选的中断信号，用于在用户取消时终止长时间运行的工具
   */
  execute(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult>;

  /**
   * 获取工具定义列表
   */
  getTools(): Promise<ExtendedToolDefinition[]>;

  /**
   * 检查工具是否可用
   */
  isAvailable(toolName: string): Promise<boolean>;
}

// ============================================================================
// MCP 相关类型
// ============================================================================

/**
 * MCP 服务器配置
 */
export interface MCPServerConfig {
  id: string;
  name: string;
  type: 'stdio' | 'sse' | 'streamable-http';
  command?: string; // stdio 类型
  args?: string[];
  env?: Record<string, string>;
  url?: string; // sse / streamable-http 类型
  headers?: Record<string, string>; // sse / streamable-http 自定义请求头
  enabled: boolean;
}

/**
 * MCP 工具
 */
export interface MCPTool {
  name: string;
  description: string;
  inputSchema: {
    type: 'object';
    properties: Record<string, any>;
    required?: string[];
  };
}

// ============================================================================
// Skill 相关类型
// ============================================================================

/**
 * Skill 定义
 */
export interface SkillDefinition {
  id: string;
  name: string;
  description: string;
  parameters: {
    type: 'object';
    properties: Record<string, any>;
    required?: string[];
  };
  handler: (args: Record<string, any>) => Promise<string>;
}

// ============================================================================
// Shell 执行器类型
// ============================================================================

/**
 * Shell 执行选项
 */
export interface ShellExecuteOptions {
  command: string;
  cwd?: string;
  timeout?: number;
  env?: Record<string, string>;
  name?: string; // 终端名称（用于VSCode终端）
  showTerminal?: boolean; // 是否显示终端窗口
  keepTerminalOpen?: boolean; // 是否保持终端打开
  useVSCodeTerminal?: boolean; // 是否使用VSCode终端（默认false，使用child_process）

}

/**
 * Shell 执行结果
 */
export interface ShellExecuteResult {
  stdout: string;
  stderr: string;
  exitCode: number | null;
  duration: number;
}

/**
 * launch-process 选项
 */
export interface LaunchProcessOptions {
  command: string;
  cwd?: string;
  wait: boolean;
  maxWaitSeconds: number;
  name: string;
  showTerminal?: boolean;
  runMode?: ProcessRunMode;
  startupWaitSeconds?: number;
  readyPatterns?: string[];
}

/**
 * process 运行模式
 * - task: 一次性命令，结束后可复用终端
 * - service: 长驻服务，终端会被锁定直至显式 kill
 */
export type ProcessRunMode = 'task' | 'service';

/**
 * process 运行阶段
 */
export type ProcessPhase =
  | 'starting'
  | 'running'
  | 'ready'
  | 'completed'
  | 'failed'
  | 'killed'
  | 'timeout';

/**
 * launch-process 结果
 */
export interface LaunchProcessResult {
  terminal_id: number;
  status: 'queued' | 'starting' | 'running' | 'completed' | 'failed' | 'killed' | 'timeout';
  output: string;
  return_code: number | null;
  run_mode: ProcessRunMode;
  phase: ProcessPhase;
  locked: boolean;
  terminal_name: string;
  cwd?: string;
  output_cursor: number;
  output_start_cursor: number;
  message?: string;
  startup_status?: 'pending' | 'confirmed' | 'timeout' | 'failed' | 'skipped';
  startup_confirmed?: boolean;
  startup_message?: string;
}

/**
 * read-process 结果
 */
export interface ReadProcessResult {
  status: 'queued' | 'starting' | 'running' | 'completed' | 'failed' | 'killed' | 'timeout';
  output: string;
  return_code: number | null;
  run_mode: ProcessRunMode;
  phase: ProcessPhase;
  locked: boolean;
  terminal_name: string;
  cwd?: string;
  from_cursor: number;
  output_start_cursor: number;
  next_cursor: number;
  delta: boolean;
  truncated: boolean;
  output_cursor: number;
}

/**
 * write-process 结果
 */
export interface WriteProcessResult {
  accepted: boolean;
  status: 'queued' | 'starting' | 'running' | 'completed' | 'failed' | 'killed' | 'timeout';
  run_mode: ProcessRunMode;
  terminal_name: string;
  message?: string;
}

/**
 * kill-process 结果
 */
export interface KillProcessResult {
  killed: boolean;
  final_output: string;
  return_code: number | null;
  run_mode?: ProcessRunMode;
  terminal_name?: string;
  released_lock?: boolean;
}
