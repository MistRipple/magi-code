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
export type DesktopUpdateCheckResult = 'latest' | 'available' | 'error' | 'ignored';

export const desktopUpdaterState = $state({
  phase: 'idle' as DesktopUpdaterPhase,
  update: null as DesktopUpdateInfo | null,
  progress: null as DesktopUpdateProgress | null,
  error: '',
  errorStage: null as DesktopUpdaterErrorStage,
  currentVersion: '',
  lastCheckedAt: 0,
  lastCheckAttemptAt: 0,
});

let started = false;
let initialCheckTimer: number | null = null;
let intervalTimer: number | null = null;
let latestResultTimer: number | null = null;
let checkPromise: Promise<DesktopUpdateCheckResult> | null = null;
const MANUAL_UPDATE_CHECK_FEEDBACK_MS = 1_000;
const MANUAL_LATEST_RESULT_DURATION_MS = 5_000;

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

function clearLatestResultTimer(): void {
  if (latestResultTimer !== null) {
    window.clearTimeout(latestResultTimer);
    latestResultTimer = null;
  }
}

function settleLatestDesktopUpdateCheck(source: 'automatic' | 'manual'): void {
  clearLatestResultTimer();
  if (source === 'automatic') {
    desktopUpdaterState.phase = 'idle';
    return;
  }

  desktopUpdaterState.phase = 'latest';
  latestResultTimer = window.setTimeout(() => {
    if (desktopUpdaterState.phase === 'latest') {
      desktopUpdaterState.phase = 'idle';
    }
    latestResultTimer = null;
  }, MANUAL_LATEST_RESULT_DURATION_MS);
}

export async function checkForDesktopUpdate(
  source: 'automatic' | 'manual' = 'manual',
): Promise<DesktopUpdateCheckResult> {
  if (!isDesktopRuntime()) return 'ignored';
  if (checkPromise) return checkPromise;
  if (
    desktopUpdaterState.phase === 'downloading'
    || desktopUpdaterState.phase === 'ready'
    || desktopUpdaterState.phase === 'installing'
  ) {
    return 'ignored';
  }

  const previousPhase = desktopUpdaterState.phase;
  checkPromise = (async () => {
    const checkingStartedAt = Date.now();
    desktopUpdaterState.lastCheckAttemptAt = Date.now();
    desktopUpdaterState.phase = 'checking';
    desktopUpdaterState.progress = null;
    desktopUpdaterState.error = '';
    desktopUpdaterState.errorStage = null;
    try {
      const nextUpdate = await checkDesktopUpdate();
      if (source === 'manual') {
        const remainingFeedbackMs = MANUAL_UPDATE_CHECK_FEEDBACK_MS - (Date.now() - checkingStartedAt);
        if (remainingFeedbackMs > 0) {
          await new Promise<void>((resolve) => window.setTimeout(resolve, remainingFeedbackMs));
        }
      }
      desktopUpdaterState.lastCheckedAt = Date.now();
      await closeCurrentUpdate();
      desktopUpdaterState.update = nextUpdate;
      if (nextUpdate) {
        clearLatestResultTimer();
        desktopUpdaterState.phase = 'available';
        return 'available';
      }
      settleLatestDesktopUpdateCheck(source);
      return 'latest';
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
      return 'error';
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
  try {
    await update.installAndRestart();
  } catch (error) {
    desktopUpdaterState.phase = 'error';
    desktopUpdaterState.error = errorMessage(error);
    desktopUpdaterState.errorStage = 'install';
  }
}

export async function retryDesktopUpdate(): Promise<DesktopUpdateCheckResult> {
  if (desktopUpdaterState.errorStage === 'check') {
    return checkForDesktopUpdate('manual');
  }
  if (desktopUpdaterState.errorStage === 'download') {
    await downloadDesktopUpdate();
    return 'ignored';
  }
  if (desktopUpdaterState.errorStage === 'install') {
    desktopUpdaterState.phase = 'ready';
    await restartWithDesktopUpdate();
  }
  return 'ignored';
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
    clearLatestResultTimer();
    document.removeEventListener('visibilitychange', handleVisibilityChange);
    window.removeEventListener('online', handleOnline);
    void closeCurrentUpdate();
  };
}
