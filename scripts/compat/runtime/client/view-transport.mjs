// 每份隔离文档只持有一组在途调用；业务状态与执行仍归 daemon。
export function createViewTransport(post) {
  const pending = new Map();
  let next = 0, disposed = false;
  const closed = () => new DOMException('插件页面已关闭', 'AbortError');
  return {
    request(message, signal) {
      if (disposed) return Promise.reject(closed());
      if (signal?.aborted) return Promise.reject(signal.reason);
      return new Promise((resolve, reject) => {
        const requestId = `r${++next}`;
        const finish = (callback, value) => {
          if (!pending.delete(requestId)) return;
          signal?.removeEventListener('abort', abort);
          callback(value);
        };
        const abort = () => {
          finish(reject, signal.reason);
          // 只有路由执行支持取消；资源/设置写入的真实提交不因停止等待而撤销。
          if (message.operation === 'route.call') post({ type: 'cancel', requestId });
        };
        pending.set(requestId, {
          resolve: value => finish(resolve, value),
          reject: error => finish(reject, error),
        });
        signal?.addEventListener('abort', abort, { once: true });
        try { post({ ...message, type: 'request', requestId }); }
        catch (error) { finish(reject, error); }
      });
    },
    receive(message) {
      const waiter = pending.get(message.requestId);
      if (waiter) message.ok ? waiter.resolve(message.result) : waiter.reject(new Error(message.error));
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      for (const waiter of pending.values()) waiter.reject(closed());
    },
  };
}
