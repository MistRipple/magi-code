/**
 * Shell 执行器抽象层类型定义
 *
 * 将 Shell 执行能力从 VSCode Terminal API 中解耦，
 * 支持 VSCode / IDEA / CLI 等不同宿主环境的实现。
 */

import {
  LaunchProcessOptions,
  LaunchProcessResult,
  ReadProcessResult,
  WriteProcessResult,
  KillProcessResult,
  ProcessRunMode,
  ProcessPhase,
} from '../types';
import { ProcessState } from '../terminal/types';

// ============================================================================
// Shell 会话抽象
// ============================================================================

/**
 * Shell 会话接口
 *
 * 对底层终端通道的最小抽象：
 * - VSCode 实现：包装 vscode.Terminal
 * - Node 实现：包装 child_process.spawn 创建的 shell 进程
 */
export interface IShellSession {
  /** 会话唯一标识 */
  readonly id: string;
  /** 显示名称 */
  readonly name: string;
  /** 向 shell 发送文本 */
  sendText(text: string, addNewLine?: boolean): void;
}

// ============================================================================
// 进程记录
// ============================================================================

/**
 * 进程记录（listProcessRecords 返回类型）
 */
export interface ProcessRecord {
  terminal_id: number;
  status: ProcessState;
  command: string;
  cwd: string | undefined;
  started_at: number;
  elapsed_seconds: number;
  run_mode: ProcessRunMode;
  phase: ProcessPhase;
  locked: boolean;
  terminal_name: string;
  return_code: number | null;
  output_cursor: number;
}

// ============================================================================
// Shell 执行器接口
// ============================================================================

/**
 * Shell 执行器接口
 *
 * ToolManager 对终端能力的唯一依赖契约。
 * 所有宿主环境的实现（VSCode / Node / IDEA）必须实现此接口。
 */
export interface IShellExecutor {
  /**
   * 校验命令安全性
   * @returns valid=false 时附带拒绝原因
   */
  validateCommand(command: string): { valid: boolean; reason?: string };

  /**
   * 启动进程
   */
  launchProcess(
    options: LaunchProcessOptions,
    signal?: AbortSignal
  ): Promise<LaunchProcessResult>;

  /**
   * 读取进程输出与状态
   */
  readProcess(
    terminalId: number,
    wait: boolean,
    maxWaitSeconds: number,
    fromCursor?: number,
    signal?: AbortSignal
  ): Promise<ReadProcessResult>;

  /**
   * 向进程写入标准输入
   */
  writeProcess(
    terminalId: number,
    inputText: string
  ): Promise<WriteProcessResult>;

  /**
   * 终止进程
   */
  killProcess(terminalId: number): Promise<KillProcessResult>;

  /**
   * 获取所有进程记录
   */
  listProcessRecords(): ProcessRecord[];

  /**
   * 释放资源
   */
  dispose(): void;
}
