// 所有生态共享的运行目录提交器。这里保留处理器注册意图；生效与授权由 daemon 裁决。
export class RuntimeCatalog {
  constructor(api) {
    this.api = api;
    this.revision = 0;
    this.catalog = { tools: [], commands: [], modelSources: [] };
    this.queue = Promise.resolve();
    this.failure = null;
    this.live = false;
  }
  enqueue(operation) {
    const work = this.queue.then(() => {
      if (this.failure) throw this.failure;
      return operation();
    });
    this.queue = work.catch((error) => {
      this.failure ??= error;
      if (this.live) this.api.fail(error);
    });
    return work;
  }
  async publish() {
    const result = await this.api.catalog.replace(this.revision, structuredClone(this.catalog));
    this.revision = result.revision;
  }
  async drain() {
    let current;
    do { current = this.queue; await current; } while (current !== this.queue);
    if (this.failure) throw this.failure;
  }
  register(kind, definition, bind) {
    let disposeBinding;
    const registration = this.enqueue(async () => {
      if (this.catalog[kind].some((item) => item.id === definition.id)) throw new Error(`运行目录重复注册 ${definition.id}`);
      this.catalog[kind].push(definition);
      await this.publish();
      disposeBinding = await bind();
    });
    // 上游同步注册 API 返回 effect disposer，异步 RPC 的失败进入同一生命周期。
    registration.catch(() => {});
    let disposed = false;
    return () => {
      if (disposed) return;
      disposed = true;
      const removal = this.enqueue(async () => {
        await disposeBinding?.();
        this.catalog[kind] = this.catalog[kind].filter((item) => item !== definition);
        await this.publish();
      });
      removal.catch(() => {});
      return removal;
    };
  }
}
