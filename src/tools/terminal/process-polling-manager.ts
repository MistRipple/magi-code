/**
 * 进程轮询管理器
 *
 * 后台定期检查活跃进程状态，用于检测命令完成
 */

import { logger, LogCategory } from '../../logging';

/**
 * 进程状态更新回调
 */
export type ProcessStateUpdateCallback = () => void;

/**
 * 进程轮询管理器
 */
export class ProcessPollingManager {
  private static readonly LOG_PREFIX = 'ProcessPollingManager';

  private pollingInterval?: NodeJS.Timeout;
  private isPolling = false;
  private readonly pollIntervalMs: number;
  private readonly updateCallback: ProcessStateUpdateCallback;

  constructor(
    updateCallback: ProcessStateUpdateCallback,
    pollIntervalMs: number = 1000
  ) {
    this.updateCallback = updateCallback;
    this.pollIntervalMs = pollIntervalMs;
  }

  /**
   * 开始轮询
   */
  startPolling(): void {
    if (this.isPolling) {
      return;
    }

    logger.debug(`${ProcessPollingManager.LOG_PREFIX}: 开始后台进程轮询`, undefined, LogCategory.SHELL);
    this.isPolling = true;

    this.pollingInterval = setInterval(() => {
      this.pollActiveProcesses();
    }, this.pollIntervalMs);
  }

  /**
   * 停止轮询
   */
  stopPolling(): void {
    if (!this.isPolling) {
      return;
    }

    logger.debug(`${ProcessPollingManager.LOG_PREFIX}: 停止后台进程轮询`, undefined, LogCategory.SHELL);
    this.isPolling = false;

    if (this.pollingInterval) {
      clearInterval(this.pollingInterval);
      this.pollingInterval = undefined;
    }
  }

  /**
   * 检查是否正在轮询
   */
  get polling(): boolean {
    return this.isPolling;
  }

  /**
   * 轮询活跃进程
   */
  private pollActiveProcesses(): void {
    try {
      this.updateCallback();
    } catch (error) {
      logger.error(`${ProcessPollingManager.LOG_PREFIX}: 轮询出错`, { error }, LogCategory.SHELL);
    }
  }

  /**
   * 清理资源
   */
  dispose(): void {
    this.stopPolling();
  }
}
