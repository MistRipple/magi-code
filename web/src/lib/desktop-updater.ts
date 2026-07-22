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

type StagedDesktopUpdate = Pick<DesktopUpdateInfo, 'currentVersion' | 'version' | 'date' | 'body'>;

type DesktopUpdateDownloadEvent =
  | { event: 'Started'; data: { contentLength?: number } }
  | { event: 'Progress'; data: { chunkLength: number } }
  | { event: 'Finished' };

export const DESKTOP_UPDATE_INITIAL_CHECK_DELAY_MS = 1_200;
export const DESKTOP_UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1_000;
export const DESKTOP_UPDATE_RETRY_INTERVAL_MS = 15 * 60 * 1_000;
// GitHub Release 刚发布时，latest.json 可能还处于资产同步窗口；检查失败时只做有限重试，避免把短暂错误直接暴露给用户。
export const DESKTOP_UPDATE_CHECK_RETRY_DELAYS_MS = [0, 1_000, 3_000] as const;

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

function waitForDesktopUpdateCheckRetry(delayMs: number): Promise<void> {
  if (delayMs <= 0) {
    return Promise.resolve();
  }
  return new Promise((resolve) => window.setTimeout(resolve, delayMs));
}

async function installStagedUpdateAndRestart(): Promise<void> {
  const { invoke } = await import('@tauri-apps/api/core');
  const { relaunch } = await import('@tauri-apps/plugin-process');
  let runtimePrepared = false;
  try {
    await invoke('prepare_update_restart');
    runtimePrepared = true;
    await invoke('install_staged_desktop_update');
    await relaunch();
  } catch (error) {
    // daemon 已经停止后若安装失败，保留已下载包并重启当前版本恢复可用状态。
    if (runtimePrepared) {
      await relaunch().catch(() => undefined);
    }
    throw error;
  }
}

function createStagedDesktopUpdate(update: StagedDesktopUpdate): DesktopUpdateInfo {
  return {
    ...update,
    download: async () => undefined,
    installAndRestart: installStagedUpdateAndRestart,
    close: async () => undefined,
  };
}

export async function checkDesktopUpdate(): Promise<DesktopUpdateInfo | null> {
  if (!isDesktopRuntime()) {
    return null;
  }

  const { invoke } = await import('@tauri-apps/api/core');
  const stagedUpdate = await invoke<StagedDesktopUpdate | null>('get_staged_desktop_update');
  if (stagedUpdate) {
    return createStagedDesktopUpdate(stagedUpdate);
  }

  const { check } = await import('@tauri-apps/plugin-updater');
  let update: Awaited<ReturnType<typeof check>> | null = null;
  let checkCompleted = false;
  let lastCheckError: unknown;
  for (const delayMs of DESKTOP_UPDATE_CHECK_RETRY_DELAYS_MS) {
    await waitForDesktopUpdateCheckRetry(delayMs);
    try {
      update = await check();
      checkCompleted = true;
      break;
    } catch (error) {
      lastCheckError = error;
    }
  }
  if (!checkCompleted) {
    throw lastCheckError ?? new Error('桌面端更新检查失败');
  }
  if (!update) {
    return null;
  }

  return {
    currentVersion: update.currentVersion,
    version: update.version,
    date: update.date,
    body: update.body,
    download: async (onProgress) => {
      const { Channel } = await import('@tauri-apps/api/core');
      let downloadedBytes = 0;
      let contentLength: number | undefined;
      const channel = new Channel<DesktopUpdateDownloadEvent>();
      channel.onmessage = (event) => {
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
      };
      await invoke('stage_desktop_update', { version: update.version, onEvent: channel });
    },
    installAndRestart: installStagedUpdateAndRestart,
    close: () => update.close(),
  };
}
