/**
 * CWD 追踪器
 *
 * 通过 Shell hooks 实时追踪终端的当前工作目录
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { ShellType } from './types';
import { logger, LogCategory } from '../../logging';

/**
 * CWD 追踪器
 */
export class CwdTracker {
  private static readonly LOG_PREFIX = 'CwdTracker';

  /**
   * 生成 CWD 追踪设置命令
   * 根据不同 shell 类型生成对应的 hook 命令
   */
  static generateCwdTrackingSetup(shellName: ShellType, cwdFile: string): string {
    switch (shellName) {
      case 'zsh':
        // zsh 使用 chpwd_functions hook
        return `__magi_chpwd_tracker() { echo "$(pwd)" > ${cwdFile}; }; chpwd_functions+=(__magi_chpwd_tracker); echo "$(pwd)" > ${cwdFile}`;

      case 'bash':
        // bash 使用 PROMPT_COMMAND
        return `{ if [ -n "$PROMPT_COMMAND" ]; then export PROMPT_COMMAND="$PROMPT_COMMAND"$'\n'"echo \\"\\$(pwd)\\" > ${cwdFile}"; else export PROMPT_COMMAND="echo \\"\\$(pwd)\\" > ${cwdFile}"; fi; echo "$(pwd)" > ${cwdFile}; }`;

      case 'fish':
        // fish 使用 PWD 变量监听
        return `{ function magi_track_cwd --on-variable PWD; echo $PWD > ${cwdFile}; end; echo (pwd) > ${cwdFile}; }`;

      default:
        return '';
    }
  }

  /**
   * 生成唯一的 CWD 追踪文件路径
   */
  static generateCwdFilePath(terminalId: string): string {
    const tmpDir = os.tmpdir();
    return path.join(tmpDir, `magi-cwd-${terminalId}.txt`);
  }

  /**
   * 读取当前 CWD
   */
  static readCurrentCwd(cwdFile: string): string | undefined {
    try {
      if (fs.existsSync(cwdFile)) {
        return fs.readFileSync(cwdFile, 'utf8').trim() || undefined;
      }
    } catch (error) {
      logger.debug(`${this.LOG_PREFIX}: 读取 CWD 失败`, { error }, LogCategory.SHELL);
    }
    return undefined;
  }

  /**
   * 清理 CWD 追踪文件
   */
  static cleanupCwdTracking(cwdFile: string): void {
    try {
      if (fs.existsSync(cwdFile)) {
        fs.unlinkSync(cwdFile);
        logger.debug(`${this.LOG_PREFIX}: 清理 CWD 追踪文件`, { cwdFile }, LogCategory.SHELL);
      }
    } catch (error) {
      logger.debug(`${this.LOG_PREFIX}: 清理 CWD 追踪文件失败`, { error }, LogCategory.SHELL);
    }
  }
}
