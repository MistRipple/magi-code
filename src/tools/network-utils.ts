/**
 * 网络请求工具（工具层统一治理）
 *
 * 目标：
 * - 统一 timeout + signal 组合逻辑，避免不同工具行为不一致
 * - 统一网络瞬态错误识别，避免将上游网络抖动误判为“插件故障”
 * - 提供可控重试（默认仅针对网络瞬态与指定状态码）
 */

export interface FetchWithRetryOptions {
  timeoutMs: number;
  attempts?: number;
  signal?: AbortSignal;
  baseDelayMs?: number;
  maxDelayMs?: number;
  retryOnStatuses?: number[];
}

const DEFAULT_RETRY_STATUSES = [429, 500, 502, 503, 504];

function createAbortError(): Error {
  try {
    return new DOMException('The operation was aborted.', 'AbortError');
  } catch {
    const error = new Error('The operation was aborted.');
    error.name = 'AbortError';
    return error;
  }
}

async function sleepWithSignal(ms: number, signal?: AbortSignal): Promise<void> {
  if (ms <= 0) {
    return;
  }
  if (signal?.aborted) {
    throw createAbortError();
  }
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      resolve();
    }, ms);

    const onAbort = () => {
      cleanup();
      reject(createAbortError());
    };

    const cleanup = () => {
      clearTimeout(timer);
      if (signal) {
        signal.removeEventListener('abort', onAbort);
      }
    };

    if (signal) {
      signal.addEventListener('abort', onAbort, { once: true });
    }
  });
}

export function toErrorMessage(error: unknown): string {
  if (!error) {
    return 'Unknown error';
  }
  if (error instanceof Error) {
    return error.message || error.name;
  }
  return String(error);
}

export function isRetryableNetworkError(message: string): boolean {
  const lower = (message || '').toLowerCase();
  return (
    lower.includes('timeout') ||
    lower.includes('timed out') ||
    lower.includes('network') ||
    lower.includes('connection') ||
    lower.includes('fetch failed') ||
    lower.includes('socket hang up') ||
    lower.includes('econnreset') ||
    lower.includes('econnrefused') ||
    lower.includes('enotfound') ||
    lower.includes('eai_again') ||
    lower.includes('tls') ||
    lower.includes('certificate') ||
    lower.includes('service unavailable') ||
    lower.includes('request aborted') ||
    lower.includes('aborted')
  );
}

export function combineSignalWithTimeout(signal: AbortSignal | undefined, timeoutMs: number): AbortSignal {
  const timeoutSignal = AbortSignal.timeout(timeoutMs);
  if (!signal) {
    return timeoutSignal;
  }
  const abortSignalWithAny = AbortSignal as typeof AbortSignal & {
    any?: (signals: AbortSignal[]) => AbortSignal;
  };
  if (typeof abortSignalWithAny.any === 'function') {
    return abortSignalWithAny.any([signal, timeoutSignal]);
  }
  // 兼容不支持 AbortSignal.any 的运行时，手动合并两个 signal
  const controller = new AbortController();
  const abortWithReason = (source: AbortSignal) => {
    if (controller.signal.aborted) {
      return;
    }
    const maybeController = controller as AbortController & { abort: (reason?: unknown) => void };
    const sourceReason = (source as AbortSignal & { reason?: unknown }).reason;
    try {
      maybeController.abort(sourceReason);
    } catch {
      controller.abort();
    }
  };

  if (signal.aborted) {
    abortWithReason(signal);
    return controller.signal;
  }
  if (timeoutSignal.aborted) {
    abortWithReason(timeoutSignal);
    return controller.signal;
  }

  const onSourceAbort = () => {
    cleanup();
    abortWithReason(signal);
  };
  const onTimeoutAbort = () => {
    cleanup();
    abortWithReason(timeoutSignal);
  };
  const cleanup = () => {
    signal.removeEventListener('abort', onSourceAbort);
    timeoutSignal.removeEventListener('abort', onTimeoutAbort);
  };

  signal.addEventListener('abort', onSourceAbort, { once: true });
  timeoutSignal.addEventListener('abort', onTimeoutAbort, { once: true });
  return controller.signal;
}

export async function fetchWithRetry(
  input: string | URL,
  init: RequestInit,
  options: FetchWithRetryOptions,
): Promise<Response> {
  const attempts = Math.max(1, options.attempts ?? 1);
  const baseDelayMs = Math.max(0, options.baseDelayMs ?? 250);
  const maxDelayMs = Math.max(baseDelayMs, options.maxDelayMs ?? 1500);
  const retryOnStatuses = new Set(options.retryOnStatuses ?? DEFAULT_RETRY_STATUSES);
  let lastError: unknown;

  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    if (options.signal?.aborted) {
      throw createAbortError();
    }

    try {
      const response = await fetch(input, {
        ...init,
        signal: combineSignalWithTimeout(options.signal, options.timeoutMs),
      });

      const shouldRetryByStatus = attempt < attempts && retryOnStatuses.has(response.status);
      if (!shouldRetryByStatus) {
        return response;
      }

      try {
        // 释放响应体，避免重试时连接资源滞留
        await response.arrayBuffer();
      } catch {
        // ignore body drain failures
      }

      const delayMs = Math.min(baseDelayMs * attempt, maxDelayMs);
      await sleepWithSignal(delayMs, options.signal);
      continue;
    } catch (error) {
      lastError = error;
      const errorMessage = toErrorMessage(error);
      const shouldRetry =
        attempt < attempts
        && isRetryableNetworkError(errorMessage)
        && !options.signal?.aborted;

      if (!shouldRetry) {
        throw error;
      }

      const delayMs = Math.min(baseDelayMs * attempt, maxDelayMs);
      await sleepWithSignal(delayMs, options.signal);
    }
  }

  throw (lastError instanceof Error ? lastError : new Error(toErrorMessage(lastError)));
}
