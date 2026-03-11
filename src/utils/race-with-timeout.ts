/**
 * Promise.race + 超时的安全包装
 *
 * 解决标准 Promise.race 超时模式的 timer 泄漏问题：
 * 当主 Promise resolve/reject 后，超时 timer 会被自动 clearTimeout。
 */

/**
 * 将 promise 与超时竞速，超时后 reject
 *
 * @param promise 要执行的 Promise
 * @param timeoutMs 超时毫秒数
 * @param timeoutMessage 超时错误消息
 * @returns Promise 结果
 */
export function raceWithTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  timeoutMessage: string = 'Operation timed out',
): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;

  const timeoutPromise = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(timeoutMessage)), timeoutMs);
  });

  return Promise.race([promise, timeoutPromise]).finally(() => {
    clearTimeout(timer);
  });
}
