export type DesktopUpdateProgress = {
  downloadedBytes: number;
  contentLength?: number;
  percent?: number;
};

export type DesktopUpdateInstallability = {
  installable: boolean;
};

export type DesktopUpdateInfo = {
  currentVersion: string;
  version: string;
  date?: string;
  body?: string;
  installability: DesktopUpdateInstallability;
  download: (onProgress?: (progress: DesktopUpdateProgress) => void) => Promise<void>;
  installAndRestart: () => Promise<void>;
  close: () => Promise<void>;
};

export const DESKTOP_UPDATE_INITIAL_CHECK_DELAY_MS = 1_200;
export const DESKTOP_UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1_000;
export const DESKTOP_UPDATE_RETRY_INTERVAL_MS = 15 * 60 * 1_000;
export const DESKTOP_UPDATE_CHECK_RETRY_DELAYS_MS = [0, 1_000, 3_000] as const;

export function isDesktopUpdateCheckDue(
  lastCheckedAt: number,
  now: number = Date.now(),
  intervalMs: number = DESKTOP_UPDATE_CHECK_INTERVAL_MS,
): boolean {
  if (!Number.isFinite(lastCheckedAt) || lastCheckedAt <= 0) return true;
  return now - lastCheckedAt >= intervalMs;
}

export function isDesktopRuntime(): boolean {
  return typeof window !== 'undefined' && window.magiDesktop?.runtime === 'electron';
}

export async function getDesktopAppVersion(): Promise<string | null> {
  return isDesktopRuntime() ? window.magiDesktop!.getAppVersion() : null;
}

export function formatUpdateProgress(
  downloadedBytes: number,
  contentLength?: number,
): DesktopUpdateProgress {
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

function waitForRetry(delayMs: number): Promise<void> {
  return delayMs <= 0
    ? Promise.resolve()
    : new Promise((resolve) => window.setTimeout(resolve, delayMs));
}

export async function checkDesktopUpdate(): Promise<DesktopUpdateInfo | null> {
  const desktop = window.magiDesktop;
  if (!isDesktopRuntime() || !desktop) return null;

  let snapshot: MagiDesktopUpdateSnapshot | null = null;
  let lastError: unknown;
  for (const delayMs of DESKTOP_UPDATE_CHECK_RETRY_DELAYS_MS) {
    await waitForRetry(delayMs);
    try {
      snapshot = await desktop.checkForUpdates();
      lastError = null;
      break;
    } catch (error) {
      lastError = error;
    }
  }
  if (!snapshot) throw lastError ?? new Error('桌面端更新检查失败');
  if (!snapshot.availableVersion) return null;

  return {
    currentVersion: snapshot.currentVersion,
    version: snapshot.availableVersion,
    installability: { installable: true },
    download: async (onProgress) => {
      const unsubscribe = desktop.onUpdate((next) => {
        if (next.status !== 'downloading' && next.status !== 'downloaded') return;
        onProgress?.(formatUpdateProgress(
          next.downloadedBytes,
          next.totalBytes ?? undefined,
        ));
      });
      try {
        const downloaded = await desktop.downloadUpdate();
        onProgress?.(formatUpdateProgress(
          downloaded.downloadedBytes,
          downloaded.totalBytes ?? downloaded.downloadedBytes,
        ));
      } finally {
        unsubscribe();
      }
    },
    installAndRestart: async () => {
      await desktop.installUpdate();
    },
    close: async () => undefined,
  };
}
