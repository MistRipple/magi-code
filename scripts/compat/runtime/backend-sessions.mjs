import { AsyncLocalStorage } from 'node:async_hooks';

// 后端 list()/get() 返回只读身份，而不是客户端的 getSnapshot()/scope()。
// 当前调用结束即失效；消息、生命周期和写入仍由 Magi daemon 拥有。
export function createBackendSessions(api) {
  const scopes = new AsyncLocalStorage();
  let disposed = false;
  const state = () => {
    const scope = scopes.getStore();
    if (disposed || !scope || scope.closed) throw new Error('会话目录只能在当前 daemon 调用内读取');
    scope.signal?.throwIfAborted();
    return scope;
  };
  const project = (record, owner) => new Proxy(Object.freeze({ id: record.sessionId }), {
    get(target, key) {
      if (state() !== owner) throw new Error('会话投影不属于当前调用');
      if (key === 'id') return target.id;
      if (key === 'then' || typeof key === 'symbol') return undefined;
      throw new Error(`后端 Session.${String(key)} 尚未映射到 Magi 原生合同`);
    },
  });
  const service = Object.freeze({
    list() { return [...state().sessions]; },
    get(id) { return state().sessions.find((session) => session.id === id); },
  });
  return {
    service,
    async run(callback, signal) {
      if (disposed) throw new Error('会话目录服务已失效');
      if (scopes.getStore()) { state(); return callback(); }
      const entries = [], ids = new Set();
      let afterSessionId;
      for (let pageNumber = 0; ; pageNumber++) {
        if (pageNumber >= 50) throw new Error('会话目录超过单次分页上限');
        signal?.throwIfAborted();
        const page = await api.session.directory({ limit: 200, ...(afterSessionId ? { afterSessionId } : {}) });
        if (disposed) throw new Error('会话目录服务已失效');
        if (!Array.isArray(page.sessions) || page.sessions.length > 200 || typeof page.hasMore !== 'boolean') throw new Error('daemon 会话目录响应无效');
        for (const entry of page.sessions) {
          if (typeof entry.sessionId !== 'string' || !entry.sessionId || ids.has(entry.sessionId)
            || !Number.isSafeInteger(entry.createdAt) || entry.createdAt < 0) throw new Error('daemon 会话目录身份重复或无效');
          ids.add(entry.sessionId); entries.push(entry);
        }
        if (entries.length > 10_000) throw new Error('会话目录超过单次投影上限');
        if (!page.hasMore) break;
        if (!page.nextAfterSessionId || page.nextAfterSessionId === afterSessionId) throw new Error('daemon 会话目录游标没有前进');
        afterSessionId = page.nextAfterSessionId;
      }
      signal?.throwIfAborted();
      const scope = { signal, closed: false, sessions: [] };
      scope.sessions = entries.sort((a, b) => a.createdAt - b.createdAt || a.sessionId.localeCompare(b.sessionId)).map(record => project(record, scope));
      return scopes.run(scope, async () => {
        try { return await callback(); }
        finally { scope.closed = true; scope.sessions = []; }
      });
    },
    dispose() { disposed = true; },
  };
}
