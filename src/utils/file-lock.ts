/**
 * 文件锁系统
 *
 * 提供跨进程的文件级锁机制
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { ConfigManager } from '../config';

/**
 * 文件锁
 */
export class FileLock {
  private lockFile: string;
  private lockDir: string;
  private acquired: boolean = false;

  constructor(resourceId: string, lockDir?: string) {
    this.lockDir = lockDir || path.join(os.tmpdir(), 'multicli-locks');
    this.lockFile = path.join(this.lockDir, `${this.sanitizeId(resourceId)}.lock`);
  }

  /**
   * 清理资源 ID，确保可以作为文件名
   */
  private sanitizeId(id: string): string {
    return id.replace(/[^a-zA-Z0-9_-]/g, '_');
  }

  /**
   * 确保锁目录存在
   */
  private ensureLockDir(): void {
    if (!fs.existsSync(this.lockDir)) {
      fs.mkdirSync(this.lockDir, { recursive: true });
    }
  }

  /**
   * 获取锁
   */
  async acquire(timeout?: number): Promise<boolean> {
    const config = ConfigManager.getInstance().get('snapshot');
    const lockTimeout = timeout || config.lockTimeout;
    const startTime = Date.now();

    this.ensureLockDir();

    while (Date.now() - startTime < lockTimeout) {
      try {
        // 使用 O_EXCL 标志确保原子性创建
        fs.writeFileSync(this.lockFile, JSON.stringify({
          pid: process.pid,
          timestamp: Date.now(),
        }), { flag: 'wx' });

        this.acquired = true;
        return true;
      } catch (error: any) {
        if (error.code === 'EEXIST') {
          // 锁文件已存在，检查是否过期
          if (this.isLockStale()) {
            this.forceRelease();
            continue;
          }

          // 等待后重试
          await this.sleep(100);
        } else {
          throw error;
        }
      }
    }

    return false;
  }

  /**
   * 释放锁
   */
  release(): void {
    if (!this.acquired) return;

    try {
      fs.unlinkSync(this.lockFile);
      this.acquired = false;
    } catch (error: any) {
      if (error.code !== 'ENOENT') {
        // 忽略文件不存在的错误
        throw error;
      }
    }
  }

  /**
   * 检查锁是否过期
   */
  private isLockStale(): boolean {
    try {
      const content = fs.readFileSync(this.lockFile, 'utf-8');
      const lockInfo = JSON.parse(content);
      const age = Date.now() - lockInfo.timestamp;

      // 锁超过 1 分钟视为过期
      return age > 60000;
    } catch {
      return true;
    }
  }

  /**
   * 强制释放锁
   */
  private forceRelease(): void {
    try {
      fs.unlinkSync(this.lockFile);
    } catch {
      // 忽略错误
    }
  }

  /**
   * 睡眠
   */
  private sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }

  /**
   * 检查锁是否已获取
   */
  isAcquired(): boolean {
    return this.acquired;
  }
}

/**
 * 锁管理器
 */
export class LockManager {
  private static instance: LockManager | null = null;
  private locks: Map<string, FileLock> = new Map();

  private constructor() {}

  /**
   * 获取单例实例
   */
  static getInstance(): LockManager {
    if (!LockManager.instance) {
      LockManager.instance = new LockManager();
    }
    return LockManager.instance;
  }

  /**
   * 获取或创建锁
   */
  getLock(resourceId: string): FileLock {
    if (!this.locks.has(resourceId)) {
      this.locks.set(resourceId, new FileLock(resourceId));
    }
    return this.locks.get(resourceId)!;
  }

  /**
   * 释放所有锁
   */
  releaseAll(): void {
    for (const lock of this.locks.values()) {
      if (lock.isAcquired()) {
        lock.release();
      }
    }
    this.locks.clear();
  }

  /**
   * 使用锁执行操作
   */
  async withLock<T>(
    resourceId: string,
    fn: () => Promise<T>,
    timeout?: number
  ): Promise<T> {
    const lock = this.getLock(resourceId);
    const acquired = await lock.acquire(timeout);

    if (!acquired) {
      throw new Error(`Failed to acquire lock for resource: ${resourceId}`);
    }

    try {
      return await fn();
    } finally {
      lock.release();
    }
  }
}
