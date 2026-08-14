import { isDesktopRuntime } from './desktop-updater';

export interface DesktopAppearance {
  backgroundColor: string;
  mode: 'light' | 'dark';
}

export async function synchronizeDesktopAppearance(appearance: DesktopAppearance): Promise<void> {
  if (!isDesktopRuntime()) return Promise.resolve();
  if (!/^#[0-9a-f]{6}$/i.test(appearance.backgroundColor)) {
    return Promise.reject(new Error('桌面壳背景色必须使用 #RRGGBB 格式'));
  }

  const desktop = window.magiDesktop;
  if (!desktop) throw new Error('desktop_preload_bridge_unavailable');
  await desktop.setAppearance(appearance);
}
