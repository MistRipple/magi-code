export type DesktopUpdateProgress = {
  downloadedBytes: number;
  contentLength?: number;
  percent?: number;
};

export type DesktopUpdateInfo = {
  currentVersion: string;
  version: string;
  date?: string;
  body?: string;
  download: (onProgress?: (progress: DesktopUpdateProgress) => void) => Promise<void>;
  installAndRestart: () => Promise<void>;
  close: () => Promise<void>;
};

export const DESKTOP_UPDATE_INITIAL_CHECK_DELAY_MS = 1_200;
export const DESKTOP_UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1_000;
export const DESKTOP_UPDATE_RETRY_INTERVAL_MS = 15 * 60 * 1_000;

export function isDesktopUpdateCheckDue(
  lastCheckedAt: number,
  now: number = Date.now(),
  intervalMs: number = DESKTOP_UPDATE_CHECK_INTERVAL_MS,
): boolean {
  if (!Number.isFinite(lastCheckedAt) || lastCheckedAt <= 0) {
    return true;
  }
  return now - lastCheckedAt >= intervalMs;
}

export function isDesktopRuntime(): boolean {
  return typeof window !== 'undefined'
    && typeof (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== 'undefined';
}

export async function getDesktopAppVersion(): Promise<string | null> {
  if (!isDesktopRuntime()) {
    return null;
  }
  const { getVersion } = await import('@tauri-apps/api/app');
  return getVersion();
}

export function formatUpdateProgress(downloadedBytes: number, contentLength?: number): DesktopUpdateProgress {
  const downloaded = Math.max(0, Math.round(downloadedBytes));
  const total = typeof contentLength === 'number' && Number.isFinite(contentLength) && contentLength > 0
    ? Math.round(contentLength)
    : undefined;
  return {
    downloadedBytes: downloaded,
    contentLength: total,
    percent: total ? Math.min(100, Math.round((downloaded / total) * 100)) : undefined,
  };
}

export async function checkDesktopUpdate(): Promise<DesktopUpdateInfo | null> {
  if (!isDesktopRuntime()) {
    return null;
  }

  const { check } = await import('@tauri-apps/plugin-updater');
  const update = await check();
  if (!update) {
    return null;
  }

  return {
    currentVersion: update.currentVersion,
    version: update.version,
    date: update.date,
    body: update.body,
    download: async (onProgress) => {
      let downloadedBytes = 0;
      let contentLength: number | undefined;
      await update.download((event) => {
        if (event.event === 'Started') {
          downloadedBytes = 0;
          contentLength = event.data.contentLength;
          onProgress?.(formatUpdateProgress(downloadedBytes, contentLength));
        } else if (event.event === 'Progress') {
          downloadedBytes += event.data.chunkLength;
          onProgress?.(formatUpdateProgress(downloadedBytes, contentLength));
        } else {
          onProgress?.(formatUpdateProgress(downloadedBytes, contentLength ?? downloadedBytes));
        }
      });
    },
    installAndRestart: async () => {
      const { invoke } = await import('@tauri-apps/api/core');
      const { relaunch } = await import('@tauri-apps/plugin-process');
      let runtimePrepared = false;
      try {
        await invoke('prepare_update_restart');
        runtimePrepared = true;
        await update.install();
        await relaunch();
      } catch (error) {
        // daemon 已经优雅停止后若安装失败，必须重启当前版本恢复可用状态。
        if (runtimePrepared) {
          await relaunch().catch(() => undefined);
        }
        throw error;
      }
    },
    close: () => update.close(),
  };
}
