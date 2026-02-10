/**
 * 终端系统类型定义
 */

import * as vscode from 'vscode';

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
 * Shell 类型
 */
export type ShellType = 'bash' | 'zsh' | 'fish' | 'powershell' | 'cmd' | 'other';

/**
 * 终端进程信息
 */
export interface TerminalProcess {
  /** 进程 ID（内部标识） */
  id: number;
  /** VSCode 终端实例 */
  terminal: vscode.Terminal;
  /** 原始命令 */
  command: string;
  /** 实际执行的命令（可能被包装） */
  actualCommand: string;
  /** 上一条命令（用于检测完成） */
  lastCommand: string;
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
  /** Shell Integration 执行对象 */
  execution?: vscode.TerminalShellExecution;
  /** 输出流 */
  readStream?: AsyncIterable<string>;
  /** 工具调用 ID */
  toolUseId?: string;
  /** 会话 ID */
  conversationId?: string;
}

// ============================================================================
// 完成检测策略
// ============================================================================

/**
 * 完成检测结果
 */
export interface CompletionCheckResult {
  /** 是否完成 */
  isCompleted: boolean;
  /** 退出码（如果可获取） */
  returnCode?: number | null;
}

/**
 * 终端会话数据（用于 ScriptCapture 策略）
 */
export interface TerminalSessionData {
  /** script 日志文件路径 */
  scriptFile: string;
  /** Shell 类型 */
  shellName: ShellType;
  /** script 进程 PID */
  scriptPid?: number;
  /** Shell 进程 PID */
  shellPid?: number;
  /** 上次文件读取位置 */
  lastFileEndPosition: number;
  /** CWD 追踪文件路径 */
  cwdTrackingFile?: string;
  /** 上次捕获的子进程集合 */
  lastChildProcesses: Set<number>;
  /** 进程检测是否进行中（防止并发） */
  processCheckInProgress?: boolean;
  /** "无子进程"稳定计数器 — 连续检测到无子进程的次数 */
  noChildStableCount: number;
}

/**
 * 完成检测策略接口
 */
export interface CompletionStrategy {
  /** 策略名称 */
  name(): string;

  /**
   * 初始化终端
   * @returns 是否成功初始化
   */
  setupTerminal(
    terminal: vscode.Terminal,
    shellName: ShellType,
    startupScript?: string
  ): Promise<boolean>;

  /**
   * 包装命令（添加标记等）
   */
  wrapCommand(
    command: string,
    processId: number,
    terminal: vscode.Terminal,
    captureOutput: boolean
  ): string;

  /**
   * 检测命令是否完成
   */
  checkCompleted(
    processId: number,
    terminal: vscode.Terminal
  ): CompletionCheckResult;

  /**
   * 获取命令输出和退出码
   */
  getOutputAndReturnCode?(
    processId: number,
    terminal: vscode.Terminal,
    actualCommand: string,
    isCompleted: boolean
  ): { output: string; returnCode: number | null } | string;

  /**
   * 获取当前工作目录
   */
  getCurrentCwd?(terminal: vscode.Terminal): string | undefined;

  /**
   * 清理终端资源
   */
  cleanupTerminal(terminal: vscode.Terminal): void;

  /**
   * 检测终端会话是否存活
   */
  isReady(terminal: vscode.Terminal): boolean;

  /**
   * 确保终端会话存活
   */
  ensureTerminalSessionActive?(
    terminal: vscode.Terminal,
    shellName?: ShellType
  ): Promise<boolean>;

  /**
   * 运行命令直到完成（用于初始化命令）
   */
  runCommandUntilCompletion(
    terminal: vscode.Terminal,
    command: string,
    description: string,
    eventName?: string,
    shellName?: ShellType,
    timeout?: number
  ): Promise<boolean>;
}

// ============================================================================
// 长时间运行终端
// ============================================================================

/**
 * 长时间运行终端信息
 */
export interface LongRunningTerminalInfo {
  /** 进程 ID */
  processId: number;
  /** 终端实例 */
  terminal: vscode.Terminal;
  /** 当前工作目录 */
  current_working_directory?: string;
}

// ============================================================================
// 终端配置
// ============================================================================

/**
 * 终端设置
 */
export interface TerminalSettings {
  /** 启动脚本 */
  startupScript?: string;
  /** 默认 Shell */
  defaultShell?: ShellType;
  /** 最大输出行数 */
  maxLinesTerminalProcessOutput?: number;
  /** 截断后保留的行数 */
  maxLinesTerminalProcessOutputAfterTruncation?: number;
}

/**
 * Shell 信息
 */
export interface ShellInfo {
  /** Shell 可执行路径 */
  path?: string;
  /** Shell 参数 */
  args?: string[];
  /** 环境变量 */
  env?: Record<string, string>;
}

// ============================================================================
// ANSI 标记常量
// ============================================================================

/**
 * 输出开始标记（ANSI OSC 序列）
 */
export const OUTPUT_START_MARKER = '\x1B]8888;magi-output-start\x07';

/**
 * 输出结束标记（ANSI OSC 序列）
 */
export const OUTPUT_END_MARKER = '\x1B]8889;magi-output-end\x07';

// ============================================================================
// Shell 通用工具函数
// ============================================================================

/**
 * 获取禁用历史展开命令
 */
export function getDisableHistExpansionCommand(shellName: ShellType): string {
  switch (shellName) {
    case 'bash':
    case 'zsh':
      return 'set +o histexpand';
    default:
      return '';
  }
}

/**
 * 获取清屏命令
 */
export function getClearCommand(shellName: ShellType): string {
  switch (shellName) {
    case 'powershell':
      return 'cls';
    default:
      return 'clear';
  }
}
