type AsyncTask = () => Promise<void>;
type AsyncTaskErrorHandler = (key: string, error: unknown) => void;

export class CoalescedAsyncTaskQueue {
  private readonly pending = new Map<string, AsyncTask>();
  private readonly running = new Map<string, Promise<void>>();

  constructor(private readonly onError?: AsyncTaskErrorHandler) {}

  schedule(key: string, task: AsyncTask): void {
    const normalizedKey = key.trim();
    if (!normalizedKey) {
      void task().catch((error) => {
        this.onError?.('__anonymous__', error);
      });
      return;
    }

    this.pending.set(normalizedKey, task);
    if (this.running.has(normalizedKey)) {
      return;
    }

    const runner = this.run(normalizedKey).finally(() => {
      this.running.delete(normalizedKey);
      if (this.pending.has(normalizedKey)) {
        this.schedule(normalizedKey, this.pending.get(normalizedKey)!);
      }
    });
    this.running.set(normalizedKey, runner);
  }

  async flushAll(): Promise<void> {
    while (this.running.size > 0 || this.pending.size > 0) {
      if (this.running.size === 0) {
        for (const [key, task] of this.pending.entries()) {
          this.schedule(key, task);
        }
      }
      await Promise.all(Array.from(this.running.values()));
    }
  }

  private async run(key: string): Promise<void> {
    while (true) {
      const task = this.pending.get(key);
      if (!task) {
        return;
      }
      this.pending.delete(key);
      try {
        await task();
      } catch (error) {
        this.onError?.(key, error);
      }
    }
  }
}

export class SerialAsyncTaskQueue {
  private readonly queues = new Map<string, Promise<void>>();

  constructor(private readonly onError?: AsyncTaskErrorHandler) {}

  enqueue(key: string, task: AsyncTask): void {
    const normalizedKey = key.trim();
    if (!normalizedKey) {
      void task().catch((error) => {
        this.onError?.('__anonymous__', error);
      });
      return;
    }

    const previous = this.queues.get(normalizedKey) || Promise.resolve();
    const next = previous.then(task, task).catch((error) => {
      this.onError?.(normalizedKey, error);
    });
    const tail = next.finally(() => {
      if (this.queues.get(normalizedKey) === tail) {
        this.queues.delete(normalizedKey);
      }
    });
    this.queues.set(normalizedKey, tail);
  }

  async flushAll(): Promise<void> {
    while (this.queues.size > 0) {
      await Promise.all(Array.from(this.queues.values()));
    }
  }
}
