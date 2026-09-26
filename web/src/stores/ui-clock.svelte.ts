const UI_CLOCK_TICK_MS = 1_000;

/**
 * UI 统一 wall-clock。
 *
 * 所有需要随真实时间推进的显示都共享这一份时间源，避免每个组件各自
 * 创建 interval，且在页面不可见时暂停、回到前台时立即校准。
 */
export const uiClockState = $state({
  now: Date.now(),
});

let consumerCount = 0;
let timer: ReturnType<typeof setTimeout> | null = null;
let visibilityListenerInstalled = false;

function clearTimer(): void {
  if (timer === null) return;
  clearTimeout(timer);
  timer = null;
}

function scheduleNextTick(): void {
  if (consumerCount === 0 || typeof document === 'undefined' || document.visibilityState !== 'visible') {
    return;
  }
  clearTimer();
  timer = setTimeout(() => {
    timer = null;
    uiClockState.now = Date.now();
    scheduleNextTick();
  }, UI_CLOCK_TICK_MS);
}

function handleVisibilityChange(): void {
  if (document.visibilityState === 'visible') {
    uiClockState.now = Date.now();
    scheduleNextTick();
  } else {
    clearTimer();
  }
}

function installVisibilityListener(): void {
  if (visibilityListenerInstalled || typeof document === 'undefined') return;
  visibilityListenerInstalled = true;
  document.addEventListener('visibilitychange', handleVisibilityChange);
}

function uninstallVisibilityListener(): void {
  if (!visibilityListenerInstalled || typeof document === 'undefined') return;
  document.removeEventListener('visibilitychange', handleVisibilityChange);
  visibilityListenerInstalled = false;
}

/** 注册一个时间显示消费者，并返回幂等释放函数。 */
export function retainUiClock(): () => void {
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    return () => undefined;
  }

  consumerCount += 1;
  if (consumerCount === 1) {
    installVisibilityListener();
    uiClockState.now = Date.now();
    scheduleNextTick();
  }

  let released = false;
  return () => {
    if (released) return;
    released = true;
    consumerCount = Math.max(0, consumerCount - 1);
    if (consumerCount === 0) {
      clearTimer();
      uninstallVisibilityListener();
    }
  };
}
