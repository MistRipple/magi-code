/**
 * Shell 执行器抽象层类型定义
 *
 * 纯命令执行模型，不依赖任何 IDE/编辑器 API。
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
// 进程事件
// ============================================================================

/**
 * Shell 执行器进程事件类型
 */
export interface ShellProcessEvents {
  /** 进程有新输出 */
  processOutput: {
    processId: number;
    output: string;
    fromCursor: number;
    cursor: number;
  };
  /** 进程进入终态 */
  processCompleted: {
    processId: number;
    state: ProcessState;
    exitCode: number | null;
    output: string;
    cursor: number;
  };
}

/** Shell 事件监听器类型 */
export type ShellEventListener<K extends keyof ShellProcessEvents> =
  (event: ShellProcessEvents[K]) => void;

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
 * 纯命令执行模型：spawn 子进程 + stdout/stderr pipe。
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
   * 注册进程事件监听器
   */
  on<K extends keyof ShellProcessEvents>(
    event: K,
    listener: ShellEventListener<K>
  ): void;

  /**
   * 移除进程事件监听器
   */
  off<K extends keyof ShellProcessEvents>(
    event: K,
    listener: ShellEventListener<K>
  ): void;

  /**
   * 释放资源
   */
  dispose(): void;
}
