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
  install: (onProgress?: (progress: DesktopUpdateProgress) => void) => Promise<void>;
  close: () => Promise<void>;
};

export function isDesktopRuntime(): boolean {
  return typeof window !== 'undefined'
    && typeof (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== 'undefined';
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
    install: async (onProgress) => {
      let downloadedBytes = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          downloadedBytes = 0;
          onProgress?.(formatUpdateProgress(downloadedBytes, event.data.contentLength));
        } else if (event.event === 'Progress') {
          downloadedBytes += event.data.chunkLength;
          onProgress?.(formatUpdateProgress(downloadedBytes));
        } else {
          onProgress?.(formatUpdateProgress(downloadedBytes, downloadedBytes));
        }
      });
      const { relaunch } = await import('@tauri-apps/plugin-process');
      await relaunch();
    },
    close: () => update.close(),
  };
}
