export class FileMutex {
  private locks: Map<string, Promise<void>> = new Map();

  /**
   * 获取指定文件路径的锁
   * 返回一个解锁函数
   */
  async acquire(filePath: string): Promise<() => void> {
    let unlockNext: () => void;

    // 创建一个新的 Promise，它将在 unlockNext 被调用时 resolve
    const nextLock = new Promise<void>((resolve) => {
      unlockNext = resolve;
    });

    // 获取当前文件的锁，如果不存在则为 Promise.resolve()
    const currentLock = this.locks.get(filePath) || Promise.resolve();

    // 将当前文件的锁更新为 nextLock，这样后续的 acquire 会等待当前的完成
    this.locks.set(filePath, currentLock.then(() => nextLock));

    // 等待前一个锁释放
    await currentLock;

    // 返回解锁函数
    return () => {
      unlockNext();
      // 如果队列里没有其他人在等（即当前锁还是 nextLock），就可以清理掉 Map
      if (this.locks.get(filePath) === nextLock) {
        this.locks.delete(filePath);
      }
    };
  }

  /**
   * 执行一个需要锁保护的异步函数
   */
  async runExclusive<T>(filePath: string, fn: () => Promise<T>): Promise<T> {
    const release = await this.acquire(filePath);
    try {
      return await fn();
    } finally {
      release();
    }
  }

  /**
   * 等待所有当前正在持有的锁释放（全局安全点）
   * 适用于跨文件的安全屏障，例如终端运行命令前
   */
  async waitForAll(): Promise<void> {
    const allLocks = Array.from(this.locks.values());
    if (allLocks.length > 0) {
      await Promise.all(allLocks);
    }
  }
}
