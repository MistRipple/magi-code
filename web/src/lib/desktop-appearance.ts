import { isDesktopRuntime } from './desktop-updater';

export interface DesktopAppearance {
  backgroundColor: string;
  mode: 'light' | 'dark';
}

let requestedSequence = 0;
let synchronizationQueue: Promise<void> = Promise.resolve();

export function synchronizeDesktopAppearance(appearance: DesktopAppearance): Promise<void> {
  if (!isDesktopRuntime()) return Promise.resolve();
  if (!/^#[0-9a-f]{6}$/i.test(appearance.backgroundColor)) {
    return Promise.reject(new Error('桌面壳背景色必须使用 #RRGGBB 格式'));
  }

  const sequence = ++requestedSequence;
  const synchronization = synchronizationQueue.then(async () => {
    if (sequence !== requestedSequence) return;
    const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow');
    if (sequence !== requestedSequence) return;

    const window = getCurrentWebviewWindow();
    await window.setTheme(appearance.mode);
    if (sequence !== requestedSequence) return;
    await window.setBackgroundColor(appearance.backgroundColor);
  });
  synchronizationQueue = synchronization.catch(() => undefined);
  return synchronization;
}
