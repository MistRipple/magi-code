import {
  DESKTOP_UPDATE_INITIAL_CHECK_DELAY_MS,
  DESKTOP_UPDATE_RETRY_INTERVAL_MS,
  checkDesktopUpdate,
  getDesktopAppVersion,
  isDesktopRuntime,
  isDesktopUpdateCheckDue,
  type DesktopUpdateInfo,
  type DesktopUpdateProgress,
} from '../lib/desktop-updater';

export type DesktopUpdaterPhase =
  | 'idle'
  | 'checking'
  | 'latest'
  | 'available'
  | 'downloading'
  | 'ready'
  | 'installing'
  | 'error';

export type DesktopUpdaterErrorStage = 'check' | 'download' | 'install' | null;

export const desktopUpdaterState = $state({
  phase: 'idle' as DesktopUpdaterPhase,
  update: null as DesktopUpdateInfo | null,
  progress: null as DesktopUpdateProgress | null,
  error: '',
  errorStage: null as DesktopUpdaterErrorStage,
  currentVersion: '',
  lastCheckedAt: 0,
  lastCheckAttemptAt: 0,
  promptDismissed: false,
});

let started = false;
let initialCheckTimer: number | null = null;
let intervalTimer: number | null = null;
let checkPromise: Promise<void> | null = null;

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function phaseAllowsAutomaticCheck(): boolean {
  return desktopUpdaterState.phase === 'idle'
    || desktopUpdaterState.phase === 'latest'
    || (desktopUpdaterState.phase === 'error' && desktopUpdaterState.errorStage === 'check');
}

async function closeCurrentUpdate(): Promise<void> {
  const current = desktopUpdaterState.update;
  desktopUpdaterState.update = null;
  if (current) {
    await current.close().catch(() => undefined);
  }
}

async function loadDesktopVersion(): Promise<void> {
  try {
    desktopUpdaterState.currentVersion = (await getDesktopAppVersion()) || '';
  } catch {
    desktopUpdaterState.currentVersion = '';
  }
}

export async function checkForDesktopUpdate(
  source: 'automatic' | 'manual' = 'manual',
): Promise<void> {
  if (!isDesktopRuntime()) return;
  if (checkPromise) return checkPromise;
  if (
    desktopUpdaterState.phase === 'downloading'
    || desktopUpdaterState.phase === 'ready'
    || desktopUpdaterState.phase === 'installing'
  ) {
    return;
  }

  const previousPhase = desktopUpdaterState.phase;
  checkPromise = (async () => {
    desktopUpdaterState.lastCheckAttemptAt = Date.now();
    desktopUpdaterState.phase = 'checking';
    desktopUpdaterState.progress = null;
    desktopUpdaterState.error = '';
    desktopUpdaterState.errorStage = null;
    try {
      const nextUpdate = await checkDesktopUpdate();
      desktopUpdaterState.lastCheckedAt = Date.now();
      await closeCurrentUpdate();
      desktopUpdaterState.update = nextUpdate;
      desktopUpdaterState.phase = nextUpdate ? 'available' : 'latest';
      desktopUpdaterState.promptDismissed = false;
    } catch (error) {
      if (source === 'automatic') {
        desktopUpdaterState.phase = previousPhase === 'latest' ? 'latest' : 'idle';
        desktopUpdaterState.error = '';
        desktopUpdaterState.errorStage = null;
      } else {
        desktopUpdaterState.phase = 'error';
        desktopUpdaterState.error = errorMessage(error);
        desktopUpdaterState.errorStage = 'check';
      }
    }
  })().finally(() => {
    checkPromise = null;
  });
  return checkPromise;
}

export async function downloadDesktopUpdate(): Promise<void> {
  const update = desktopUpdaterState.update;
  if (!update || !['available', 'error'].includes(desktopUpdaterState.phase)) return;
  if (desktopUpdaterState.phase === 'error' && desktopUpdaterState.errorStage === 'check') return;

  desktopUpdaterState.phase = 'downloading';
  desktopUpdaterState.progress = null;
  desktopUpdaterState.error = '';
  desktopUpdaterState.errorStage = null;
  desktopUpdaterState.promptDismissed = false;
  try {
    await update.download((progress) => {
      desktopUpdaterState.progress = progress;
    });
    desktopUpdaterState.phase = 'ready';
    desktopUpdaterState.progress = {
      ...(desktopUpdaterState.progress ?? { downloadedBytes: 0 }),
      percent: 100,
    };
  } catch (error) {
    desktopUpdaterState.phase = 'error';
    desktopUpdaterState.error = errorMessage(error);
    desktopUpdaterState.errorStage = 'download';
  }
}

export async function restartWithDesktopUpdate(): Promise<void> {
  const update = desktopUpdaterState.update;
  if (!update || desktopUpdaterState.phase !== 'ready') return;

  desktopUpdaterState.phase = 'installing';
  desktopUpdaterState.error = '';
  desktopUpdaterState.errorStage = null;
  desktopUpdaterState.promptDismissed = false;
  try {
    await update.installAndRestart();
  } catch (error) {
    desktopUpdaterState.phase = 'error';
    desktopUpdaterState.error = errorMessage(error);
    desktopUpdaterState.errorStage = 'install';
  }
}

export function dismissDesktopUpdatePrompt(): void {
  desktopUpdaterState.promptDismissed = true;
}

export function showDesktopUpdatePrompt(): void {
  desktopUpdaterState.promptDismissed = false;
}

export async function retryDesktopUpdate(): Promise<void> {
  if (desktopUpdaterState.errorStage === 'check') {
    await checkForDesktopUpdate('manual');
    return;
  }
  if (desktopUpdaterState.errorStage === 'download') {
    await downloadDesktopUpdate();
    return;
  }
  if (desktopUpdaterState.errorStage === 'install') {
    desktopUpdaterState.phase = 'ready';
    await restartWithDesktopUpdate();
  }
}

function checkAutomaticallyIfDue(ignoreRetryDelay = false): void {
  if (!phaseAllowsAutomaticCheck()) return;
  if (!isDesktopUpdateCheckDue(desktopUpdaterState.lastCheckedAt)) return;
  if (
    !ignoreRetryDelay
    && !isDesktopUpdateCheckDue(
      desktopUpdaterState.lastCheckAttemptAt,
      Date.now(),
      DESKTOP_UPDATE_RETRY_INTERVAL_MS,
    )
  ) {
    return;
  }
  void checkForDesktopUpdate('automatic');
}

export function startDesktopUpdater(): () => void {
  if (!isDesktopRuntime() || started) {
    return () => undefined;
  }
  started = true;
  void loadDesktopVersion();

  initialCheckTimer = window.setTimeout(
    checkAutomaticallyIfDue,
    DESKTOP_UPDATE_INITIAL_CHECK_DELAY_MS,
  );
  intervalTimer = window.setInterval(
    checkAutomaticallyIfDue,
    DESKTOP_UPDATE_RETRY_INTERVAL_MS,
  );

  const handleVisibilityChange = () => {
    if (document.visibilityState === 'visible') {
      checkAutomaticallyIfDue();
    }
  };
  const handleOnline = () => checkAutomaticallyIfDue(true);
  document.addEventListener('visibilitychange', handleVisibilityChange);
  window.addEventListener('online', handleOnline);

  return () => {
    started = false;
    if (initialCheckTimer !== null) window.clearTimeout(initialCheckTimer);
    if (intervalTimer !== null) window.clearInterval(intervalTimer);
    initialCheckTimer = null;
    intervalTimer = null;
    document.removeEventListener('visibilitychange', handleVisibilityChange);
    window.removeEventListener('online', handleOnline);
    void closeCurrentUpdate();
  };
}
