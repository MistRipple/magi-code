/**
 * 按 key 合并并发的相同请求：同一个 key 已有请求在途时直接共用它的结果。
 *
 * `forced` 表示这次调用要求“重新计算”：在途的如果不是强制的，就不能满足它，
 * 需要另起一个；在途的是强制的（或本次不要求强制）就共用。
 */
export function createSingleFlight<T>() {
  const inFlight = new Map<string, { promise: Promise<T>; forced: boolean }>();

  return function run(key: string, forced: boolean, start: () => Promise<T>): Promise<T> {
    const existing = inFlight.get(key);
    if (existing && (existing.forced || !forced)) {
      return existing.promise;
    }
    const promise = start().finally(() => {
      if (inFlight.get(key)?.promise === promise) {
        inFlight.delete(key);
      }
    });
    inFlight.set(key, { promise, forced });
    return promise;
  };
}
