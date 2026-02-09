/**
 * Script Capture 完成检测策略
 *
 * 使用 `script` 命令捕获终端输出到临时文件
 * 通过 ANSI 标记检测命令边界和完成状态
 *
 * 参考 Augment 插件实现
 */

import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import * as crypto from 'crypto';
import { spawnSync } from 'child_process';
import {
  CompletionStrategy,
  CompletionCheckResult,
  ShellType,
  TerminalSessionData,
  OUTPUT_START_MARKER,
  OUTPUT_END_MARKER,
} from './types';
import { CwdTracker } from './cwd-tracker';
import { logger, LogCategory } from '../../logging';

/**
 * 获取禁用历史展开命令
 */
function getDisableHistExpansionCommand(shellName: ShellType): string {
  switch (shellName.toLowerCase()) {
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
function getClearCommand(shellName: ShellType): string {
  switch (shellName.toLowerCase()) {
    case 'bash':
    case 'zsh':
    case 'fish':
      return 'clear';
    case 'powershell':
      return 'cls';
    default:
      return 'clear';
  }
}

/**
 * 生成输出开始标记设置命令
 */
function getOutputStartMarkerSetup(shellName: ShellType): string {
  switch (shellName.toLowerCase()) {
    case 'zsh':
      return `{ __multicli_output_marker() { printf $'\\x1b]8888;augment-output-start\\x07'; }; preexec_functions+=(__multicli_output_marker); }`;
    case 'bash':
      return `{ __multicli_ps0_func() { printf $'\\x1b]8888;augment-output-start\\x07'; }; PS0='$(__multicli_ps0_func)'"$PS0"; }`;
    case 'fish':
      return `{ function __multicli_preexec --on-event fish_preexec; printf '\\x1b]8888;augment-output-start\\x07'; end; }`;
    default:
      return '';
  }
}

/**
 * 生成输出结束标记设置命令
 */
function getOutputEndMarkerSetup(shellName: ShellType): string {
  switch (shellName.toLowerCase()) {
    case 'zsh':
      return `{ __multicli_output_end_marker() { printf $'\\x1b]8889;augment-output-end\\x07'; }; precmd_functions+=(__multicli_output_end_marker); }`;
    case 'bash':
      return `{ __multicli_output_end_marker() { printf $'\\x1b]8889;augment-output-end\\x07'; }; if [ -n "$PROMPT_COMMAND" ]; then export PROMPT_COMMAND="$PROMPT_COMMAND"$'\\n'"__multicli_output_end_marker"; else export PROMPT_COMMAND="__multicli_output_end_marker"; fi; }`;
    case 'fish':
      return `{ function __multicli_postexec --on-event fish_postexec; printf '\\x1b]8889;augment-output-end\\x07'; end; }`;
    default:
      return '';
  }
}

/**
 * 清理 ANSI 转义序列
 */
function stripAnsiCodes(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\].*?\x07)/g, '');
}

/**
 * Script Capture 完成检测策略
 */
export class ScriptCaptureStrategy implements CompletionStrategy {
  private static readonly LOG_PREFIX = 'ScriptCaptureStrategy';

  private terminalSessions: Map<vscode.Terminal, TerminalSessionData> = new Map();

  name(): string {
    return 'script_capture';
  }

  async setupTerminal(
    terminal: vscode.Terminal,
    shellName: ShellType,
    startupScript?: string
  ): Promise<boolean> {
    logger.debug(
      `${ScriptCaptureStrategy.LOG_PREFIX}: 初始化终端`,
      { shellName },
      LogCategory.SHELL
    );

    // 生成临时文件路径
    const tmpDir = os.tmpdir();
    const uniqueId = crypto.randomBytes(8).toString('hex');
    const scriptFile = path.join(tmpDir, `multicli-script-${uniqueId}.log`);
    const cwdFile = path.join(tmpDir, `multicli-cwd-${uniqueId}.txt`);

    // 生成并执行 script 启动命令
    const scriptStartCmd = this.generateScriptStartCommand(shellName, scriptFile);
    terminal.sendText(scriptStartCmd, true);

    // 等待 script 进程就绪
    const scriptReady = await this.waitForScriptReady(scriptStartCmd, scriptFile, shellName);

    let scriptPid: number | undefined;
    let shellPid: number | undefined;

    if (scriptReady) {
      scriptPid = scriptReady.scriptPid;
      shellPid = scriptReady.shellPid;
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: script 进程就绪`,
        { scriptPid, shellPid },
        LogCategory.SHELL
      );
    } else {
      logger.warn(
        `${ScriptCaptureStrategy.LOG_PREFIX}: script 进程启动失败`,
        undefined,
        LogCategory.SHELL
      );
    }

    // 保存会话数据
    this.terminalSessions.set(terminal, {
      scriptFile,
      shellName,
      scriptPid,
      shellPid,
      lastFileEndPosition: 0,
      cwdTrackingFile: cwdFile,
      lastChildProcesses: new Set(),
      noChildStableCount: 0,
    });

    // 执行启动脚本
    if (startupScript) {
      await this.runCommandUntilCompletion(terminal, startupScript, 'startup script', undefined, shellName);
    }

    // 禁用历史展开
    const disableHistCmd = getDisableHistExpansionCommand(shellName);
    if (disableHistCmd) {
      await this.runCommandUntilCompletion(terminal, disableHistCmd, 'disable history expansion', undefined, shellName, 500);
    }

    if (scriptPid === undefined || shellPid === undefined) {
      const clearCmd = getClearCommand(shellName);
      await this.runCommandUntilCompletion(terminal, clearCmd, 'clear command', undefined, shellName);
      return false;
    }

    // 设置输出结束标记 hook
    const endMarkerSetup = getOutputEndMarkerSetup(shellName);
    if (endMarkerSetup) {
      await this.runCommandUntilCompletion(terminal, endMarkerSetup, 'output end marker setup', undefined, shellName);
    }

    // 设置 CWD 追踪
    const cwdSetup = CwdTracker.generateCwdTrackingSetup(shellName, cwdFile);
    if (cwdSetup) {
      await this.runCommandUntilCompletion(terminal, cwdSetup, 'CWD tracking setup', undefined, shellName);
    }

    // 设置输出开始标记 hook
    const startMarkerSetup = getOutputStartMarkerSetup(shellName);
    if (startMarkerSetup) {
      await this.runCommandUntilCompletion(terminal, startMarkerSetup, 'output start marker setup', undefined, shellName);
    }

    // 清屏
    const clearCmd = getClearCommand(shellName);
    await this.runCommandUntilCompletion(terminal, clearCmd, 'clear command', undefined, shellName);

    return scriptPid !== undefined && shellPid !== undefined;
  }

  wrapCommand(
    command: string,
    processId: number,
    terminal: vscode.Terminal,
    captureOutput: boolean
  ): string {
    const session = this.terminalSessions.get(terminal);
    if (!session) {
      logger.warn(`${ScriptCaptureStrategy.LOG_PREFIX}: 未找到终端会话`, undefined, LogCategory.SHELL);
      return command;
    }

    // 更新文件位置
    if (session.scriptFile && captureOutput) {
      try {
        if (fs.existsSync(session.scriptFile)) {
          const stats = fs.statSync(session.scriptFile);
          session.lastFileEndPosition = stats.size;
          logger.debug(
            `${ScriptCaptureStrategy.LOG_PREFIX}: 更新文件位置`,
            { position: stats.size, processId },
            LogCategory.SHELL
          );
        }
      } catch (error) {
        logger.debug(
          `${ScriptCaptureStrategy.LOG_PREFIX}: 更新文件位置失败`,
          { error },
          LogCategory.SHELL
        );
      }
    }

    // 捕获子进程快照
    if (session.shellPid && captureOutput) {
      this.captureChildProcessesSnapshot(session.shellPid, session);
      session.noChildStableCount = 0;
    }

    // ScriptCapture 策略不需要包装命令，依赖 hook 注入标记
    return command;
  }

  checkCompleted(
    processId: number,
    terminal: vscode.Terminal
  ): CompletionCheckResult {
    const session = this.terminalSessions.get(terminal);

    // 先尝试基于标记的检测
    const markerResult = this.checkCompletedMarkerBased(processId, session);
    if (markerResult.isCompleted) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 标记检测发现完成`,
        { processId },
        LogCategory.SHELL
      );
      return markerResult;
    }

    // 再尝试基于进程的检测
    const processResult = this.checkCompletedProcessBased(processId, session);
    if (processResult.isCompleted) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 进程检测发现完成`,
        { processId },
        LogCategory.SHELL
      );
    }

    return processResult;
  }

  getOutputAndReturnCode(
    _processId: number,
    terminal: vscode.Terminal,
    actualCommand: string,
    isCompleted: boolean
  ): { output: string; returnCode: number | null } | string {
    const session = this.terminalSessions.get(terminal);
    if (!session?.scriptFile) {
      return { output: '', returnCode: null };
    }

    try {
      if (!fs.existsSync(session.scriptFile)) {
        return 'Script 日志文件不存在';
      }

      const content = fs.readFileSync(session.scriptFile, 'utf8');
      const output = this.extractOutputFromScriptLog(content, actualCommand, session.shellName);

      return {
        output: stripAnsiCodes(output).trim(),
        returnCode: isCompleted ? 0 : null,
      };
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 读取输出失败`,
        { error },
        LogCategory.SHELL
      );
      return `读取输出失败: ${error}`;
    }
  }

  getCurrentCwd(terminal: vscode.Terminal): string | undefined {
    const session = this.terminalSessions.get(terminal);
    if (session?.cwdTrackingFile) {
      return CwdTracker.readCurrentCwd(session.cwdTrackingFile);
    }
    return undefined;
  }

  cleanupTerminal(terminal: vscode.Terminal): void {
    logger.debug(
      `${ScriptCaptureStrategy.LOG_PREFIX}: 清理终端`,
      { name: terminal.name },
      LogCategory.SHELL
    );

    const session = this.terminalSessions.get(terminal);
    if (!session) {
      return;
    }

    // 终止 script 进程
    if (session.scriptPid) {
      this.killScriptProcess(session.scriptPid);
    }

    // 清理日志文件
    if (session.scriptFile) {
      try {
        if (fs.existsSync(session.scriptFile)) {
          fs.unlinkSync(session.scriptFile);
        }
      } catch (error) {
        logger.warn(
          `${ScriptCaptureStrategy.LOG_PREFIX}: 清理日志文件失败，可能导致临时文件累积`,
          { scriptFile: session.scriptFile, error },
          LogCategory.SHELL
        );
      }
    }

    // 清理 CWD 追踪文件
    if (session.cwdTrackingFile) {
      CwdTracker.cleanupCwdTracking(session.cwdTrackingFile);
    }

    this.terminalSessions.delete(terminal);
  }

  isReady(terminal: vscode.Terminal): boolean {
    const session = this.terminalSessions.get(terminal);
    if (!session?.scriptPid) {
      return false;
    }

    // 检查 script 进程是否存活
    try {
      process.kill(session.scriptPid, 0);
      return true;
    } catch (error: any) {
      // ESRCH 表示进程不存在
      if (error.code === 'ESRCH') {
        return false;
      }
      // EPERM 表示进程存在但无权限检查，仍视为就绪
      if (error.code === 'EPERM') {
        logger.debug(
          `${ScriptCaptureStrategy.LOG_PREFIX}: 无权限检查进程，假设仍在运行`,
          { scriptPid: session.scriptPid },
          LogCategory.SHELL
        );
        return true;
      }
      // 其他错误记录日志
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 检查进程状态失败`,
        { scriptPid: session.scriptPid, error },
        LogCategory.SHELL
      );
      return false;
    }
  }

  async ensureTerminalSessionActive(
    terminal: vscode.Terminal,
    shellName?: ShellType
  ): Promise<boolean> {
    if (this.isReady(terminal)) {
      return true;
    }

    logger.debug(
      `${ScriptCaptureStrategy.LOG_PREFIX}: 会话已失效，重新初始化`,
      undefined,
      LogCategory.SHELL
    );

    const session = this.terminalSessions.get(terminal);
    const shell = shellName || session?.shellName || 'bash';

    terminal.sendText('reset');
    return this.setupTerminal(terminal, shell);
  }

  async runCommandUntilCompletion(
    terminal: vscode.Terminal,
    command: string,
    description: string,
    _eventName?: string,
    _shellName?: ShellType,
    timeout: number = 1000
  ): Promise<boolean> {
    const startTime = Date.now();
    const wrappedCmd = this.wrapCommand(command, startTime, terminal, true);

    terminal.sendText(wrappedCmd, false);
    terminal.sendText('');

    await this.delay(200);

    return new Promise(resolve => {
      const interval = setInterval(() => {
        try {
          const result = this.checkCompleted(startTime, terminal);
          if (result.isCompleted) {
            clearInterval(interval);
            resolve(true);
            logger.debug(
              `${ScriptCaptureStrategy.LOG_PREFIX}: ${description} 完成`,
              { command },
              LogCategory.SHELL
            );
            return;
          }

          if (Date.now() - startTime > timeout) {
            clearInterval(interval);
            logger.warn(
              `${ScriptCaptureStrategy.LOG_PREFIX}: ${description} 超时`,
              { command },
              LogCategory.SHELL
            );
            resolve(false);
          }
        } catch (error) {
          logger.debug(
            `${ScriptCaptureStrategy.LOG_PREFIX}: 轮询出错`,
            { error },
            LogCategory.SHELL
          );
        }
      }, 100);
    });
  }

  /**
   * 检查进程检测是否进行中
   */
  isProcessCheckInProgress(terminal: vscode.Terminal): boolean {
    const session = this.terminalSessions.get(terminal);
    return session?.processCheckInProgress === true;
  }

  // ============================================================================
  // 私有方法
  // ============================================================================

  private generateScriptStartCommand(shellName: ShellType, scriptFile: string): string {
    const platform = process.platform;

    if (platform === 'darwin') {
      // macOS: script -q 静默模式
      return `script -q ${scriptFile} ${shellName}`;
    } else if (platform === 'linux') {
      // Linux: script -q -c 执行指定 shell
      return `script -q -c ${shellName} ${scriptFile}`;
    }

    // 其他平台不支持
    return '';
  }

  private async waitForScriptReady(
    _command: string,
    scriptFile: string,
    shellName: ShellType
  ): Promise<{ scriptPid: number; shellPid: number } | null> {
    const maxAttempts = 10;
    let scriptPid: number | null = null;

    // 阶段1：等待 script PID 和文件
    for (let i = 0; i < maxAttempts; i++) {
      const fileExists = fs.existsSync(scriptFile);
      const foundPid = this.findScriptPid(scriptFile);

      if (foundPid && fileExists) {
        scriptPid = foundPid;
        break;
      }

      await this.delay(500);
    }

    if (!scriptPid) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: script PID 未找到`,
        { shellName },
        LogCategory.SHELL
      );
      return null;
    }

    // 阶段2：等待子 shell PID
    const maxShellAttempts = 20;
    for (let i = 0; i < maxShellAttempts; i++) {
      const shellPid = this.findShellPid(scriptPid);
      if (shellPid) {
        return { scriptPid, shellPid };
      }

      await this.delay(500);
    }

    logger.debug(
      `${ScriptCaptureStrategy.LOG_PREFIX}: shell PID 未找到`,
      { scriptPid },
      LogCategory.SHELL
    );
    return null;
  }

  private findScriptPid(scriptFile: string): number | null {
    try {
      // 使用 pgrep 查找 script 进程
      const result = spawnSync('pgrep', ['-f', `script.*${path.basename(scriptFile)}`], {
        encoding: 'utf8',
        timeout: 5000,
      });

      if (result.status === 0 && result.stdout) {
        const pids = result.stdout.trim().split('\n').map(p => parseInt(p, 10)).filter(p => !isNaN(p));
        return pids[0] || null;
      }
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 查找 script PID 失败`,
        { error },
        LogCategory.SHELL
      );
    }
    return null;
  }

  private findShellPid(scriptPid: number): number | null {
    try {
      const result = spawnSync('pgrep', ['-P', scriptPid.toString()], {
        encoding: 'utf8',
        timeout: 5000,
      });

      if (result.status === 0 && result.stdout) {
        const pids = result.stdout.trim().split('\n').map(p => parseInt(p, 10)).filter(p => !isNaN(p));
        return pids[0] || null;
      }
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 查找 shell PID 失败`,
        { error },
        LogCategory.SHELL
      );
    }
    return null;
  }

  private killScriptProcess(pid: number): void {
    try {
      process.kill(pid, 'SIGTERM');
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 终止 script 进程`,
        { pid },
        LogCategory.SHELL
      );
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 终止 script 进程失败`,
        { pid, error },
        LogCategory.SHELL
      );
    }
  }

  private captureChildProcessesSnapshot(shellPid: number, session: TerminalSessionData): void {
    try {
      const result = spawnSync('pgrep', ['-P', shellPid.toString()], {
        encoding: 'utf8',
        timeout: 5000,
      });

      if (result.status === 0 && result.stdout) {
        const pids = result.stdout.trim().split('\n')
          .map(p => parseInt(p, 10))
          .filter(p => !isNaN(p));
        session.lastChildProcesses = new Set(pids);
      }
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 捕获子进程快照失败`,
        { shellPid, error },
        LogCategory.SHELL
      );
    }
  }

  private hasScriptFileGrown(processId: number, session?: TerminalSessionData): { hasGrown: boolean; currentFileSize?: number } {
    if (!session?.scriptFile || session.lastFileEndPosition === undefined) {
      return { hasGrown: false };
    }

    try {
      if (!fs.existsSync(session.scriptFile)) {
        return { hasGrown: false };
      }

      const currentSize = fs.statSync(session.scriptFile).size;

      if (currentSize < session.lastFileEndPosition) {
        logger.debug(
          `${ScriptCaptureStrategy.LOG_PREFIX}: 文件缩小`,
          { processId, current: currentSize, last: session.lastFileEndPosition },
          LogCategory.SHELL
        );
        return { hasGrown: false, currentFileSize: currentSize };
      }

      if (currentSize === session.lastFileEndPosition) {
        return { hasGrown: false, currentFileSize: currentSize };
      }

      return { hasGrown: true, currentFileSize: currentSize };
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 检查文件大小失败`,
        { processId, scriptFile: session?.scriptFile, error },
        LogCategory.SHELL
      );
      return { hasGrown: false };
    }
  }

  private checkCompletedMarkerBased(
    processId: number,
    session?: TerminalSessionData
  ): CompletionCheckResult {
    if (!session?.scriptFile) {
      return { isCompleted: false };
    }

    const grown = this.hasScriptFileGrown(processId, session);
    if (!grown.hasGrown) {
      return { isCompleted: false };
    }

    try {
      const startPos = session.lastFileEndPosition ?? 0;
      const endPos = grown.currentFileSize!;

      if (endPos <= startPos) {
        return { isCompleted: false };
      }

      const fd = fs.openSync(session.scriptFile, 'r');
      const length = endPos - startPos;
      const buffer = Buffer.alloc(length);
      fs.readSync(fd, buffer, 0, length, startPos);
      fs.closeSync(fd);

      const content = buffer.toString('utf8');
      if (content.includes(OUTPUT_END_MARKER)) {
        return { isCompleted: true };
      }

      return { isCompleted: false };
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 标记检测失败`,
        { processId, error },
        LogCategory.SHELL
      );
      return { isCompleted: false };
    }
  }

  private checkCompletedProcessBased(
    processId: number,
    session?: TerminalSessionData
  ): CompletionCheckResult {
    if (!session?.shellPid) {
      return { isCompleted: false };
    }

    session.processCheckInProgress = true;

    try {
      const result = spawnSync('pgrep', ['-P', session.shellPid.toString()], {
        encoding: 'utf8',
        timeout: 5000,
      });

      const currentChildren = new Set<number>();
      if (result.status === 0 && result.stdout) {
        result.stdout.trim().split('\n')
          .map(p => parseInt(p, 10))
          .filter(p => !isNaN(p))
          .forEach(p => currentChildren.add(p));
      }

      const hadChildren = session.lastChildProcesses.size > 0;
      const hasChildren = currentChildren.size > 0;

      if (hasChildren) {
        // 有子进程在运行 — 重置稳定计数器，更新快照
        session.noChildStableCount = 0;
        session.lastChildProcesses = currentChildren;
        session.processCheckInProgress = false;
        return { isCompleted: false };
      }

      // 当前没有子进程
      if (hadChildren) {
        // 之前有子进程、现在消失了 → 命令完成
        session.processCheckInProgress = false;
        return { isCompleted: true };
      }

      // 之前也没有子进程（快速命令场景）— 使用稳定计数器
      // 检查 script 文件是否曾经增长过（说明命令确实执行了）
      const grown = this.hasScriptFileGrown(processId, session);
      if (grown.hasGrown) {
        // 文件增长了但没有子进程 → 增加稳定计数
        session.noChildStableCount++;
        // 连续 3 次（~600ms）确认无子进程且文件已增长 → 认为完成
        if (session.noChildStableCount >= 3) {
          session.processCheckInProgress = false;
          return { isCompleted: true };
        }
      }

      session.lastChildProcesses = currentChildren;
      session.processCheckInProgress = false;
      return { isCompleted: false };
    } catch (error) {
      logger.debug(
        `${ScriptCaptureStrategy.LOG_PREFIX}: 进程完成检测失败`,
        { processId, shellPid: session?.shellPid, error },
        LogCategory.SHELL
      );
      session.processCheckInProgress = false;
      return { isCompleted: false };
    }
  }

  private extractOutputFromScriptLog(content: string, command: string, shellName: ShellType): string {
    // 查找输出开始和结束标记之间的内容
    const startMarkerIndex = content.lastIndexOf(OUTPUT_START_MARKER);
    const endMarkerIndex = content.lastIndexOf(OUTPUT_END_MARKER);

    if (startMarkerIndex !== -1 && endMarkerIndex !== -1 && endMarkerIndex > startMarkerIndex) {
      return content.substring(startMarkerIndex + OUTPUT_START_MARKER.length, endMarkerIndex);
    }

    // 备用：返回文件尾部内容
    const lines = content.split('\n');
    const lastLines = lines.slice(-50);
    return lastLines.join('\n');
  }

  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
