// 所有生态共享的运行目录提交器。这里保留处理器注册意图；生效与授权由 daemon 裁决。
export class RuntimeCatalog {
  constructor(api) {
    this.api = api;
    this.revision = 0;
    this.catalog = { tools: [], commands: [], modelSources: [] };
    this.queue = Promise.resolve();
    this.failure = null;
    this.live = false;
    this.changes = [];
    this.scheduled = null;
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
    do { await this.scheduled; current = this.queue; await current; } while (this.scheduled || current !== this.queue);
    if (this.failure) throw this.failure;
  }
  register(kind, definition, bind) {
    const entry = { kind, definition, bind, disposed: false, release: undefined };
    this.changes.push({ add: true, entry });
    this.schedule();
    return () => {
      if (entry.disposed) return;
      entry.disposed = true;
      this.changes.push({ add: false, entry });
      return this.schedule();
    };
  }
  schedule() {
    if (this.scheduled) return this.scheduled;
    // 同一批发现/回收一次提交完整目录，避免 N 个工具造成 N 次全目录复制与 RPC。
    const work = Promise.resolve().then(() => {
      this.scheduled = null;
      const changes = this.changes.splice(0);
      return this.enqueue(async () => {
        const additions = [];
        for (const { add, entry } of changes) {
          const { kind, definition } = entry;
          if (!add) {
            await entry.release?.();
            this.catalog[kind] = this.catalog[kind].filter((item) => item !== definition);
          } else if (!entry.disposed) {
            if (this.catalog[kind].some((item) => item.id === definition.id)) throw new Error(`运行目录重复注册 ${definition.id}`);
            this.catalog[kind].push(definition);
            additions.push(entry);
          }
        }
        await this.publish();
        for (const entry of additions) if (!entry.disposed) entry.release = await entry.bind();
      });
    });
    this.scheduled = work;
    work.catch(() => {});
    return work;
  }
}
