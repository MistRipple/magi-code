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
   */
  execute(toolCall: ToolCall): Promise<ToolResult>;

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
  type: 'stdio' | 'sse';
  command?: string; // stdio 类型
  args?: string[];
  env?: Record<string, string>;
  url?: string; // sse 类型
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
}

/**
 * launch-process 结果
 */
export interface LaunchProcessResult {
  terminal_id: number;
  status: 'queued' | 'starting' | 'running' | 'completed' | 'failed' | 'killed' | 'timeout';
  output: string;
  return_code: number | null;
}

/**
 * read-process 结果
 */
export interface ReadProcessResult {
  status: 'queued' | 'starting' | 'running' | 'completed' | 'failed' | 'killed' | 'timeout';
  output: string;
  return_code: number | null;
  cwd?: string;
}

/**
 * write-process 结果
 */
export interface WriteProcessResult {
  accepted: boolean;
  status: 'queued' | 'starting' | 'running' | 'completed' | 'failed' | 'killed' | 'timeout';
}

/**
 * kill-process 结果
 */
export interface KillProcessResult {
  killed: boolean;
  final_output: string;
  return_code: number | null;
}
