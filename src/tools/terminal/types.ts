/**
 * 终端系统类型定义
 */

import { ProcessRunMode } from '../types';

// ============================================================================
// 终端进程状态
// ============================================================================

/**
 * 进程状态
 */
export type ProcessState =
  | 'queued'
  | 'starting'
  | 'running'
  | 'completed'
  | 'failed'
  | 'killed'
  | 'timeout';

/**
 * 终端进程信息
 *
 * 纯命令执行模型：每次 process_launch 启动一个独立子进程（spawn），
 * 通过 stdout/stderr pipe 捕获输出，不依赖交互式 shell 或 PTY。
 */
export interface TerminalProcess {
  /** 进程 ID（内部标识） */
  id: number;
  /** 启动时的工作目录 */
  cwd?: string;
  /** 原始命令 */
  command: string;
  /** 命令输出 */
  output: string;
  /** 进程状态 */
  state: ProcessState;
  /** 退出码 */
  exitCode: number | null;
  /** 开始时间 */
  startTime: number;
  /** 结束时间 */
  endTime?: number;
  /** 最后更新时间 */
  updatedAt?: number;
  /** 运行模式（task/service） */
  runMode: ProcessRunMode;
  /** 归属 agent 名称 */
  agentName: string;
  /** 终端显示名称 */
  terminalName: string;
  /** service 模式下是否持有终端锁 */
  serviceLocked: boolean;
  /** 当前累计输出字符游标 */
  outputCursor: number;
  /** 输出缓冲区起始游标（用于增量读取与截断） */
  outputStartCursor: number;
}
