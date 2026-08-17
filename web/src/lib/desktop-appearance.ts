import { isDesktopRuntime } from './desktop-updater';

export interface DesktopAppearance {
  backgroundColor: string;
  accentColor: string;
  material: 'clear' | 'translucent' | 'immersive';
  mode: 'light' | 'dark';
}

// Electron 原生窗口颜色必须是可序列化的 CSS 颜色。背景色和强调色
// 共用同一校验，避免主题切换时只有 Renderer 成功而原生外壳拒绝更新。
const DESKTOP_COLOR_PATTERN = /^(?:#[0-9a-f]{6}(?:[0-9a-f]{2})?|rgba?\(\s*(?:\d{1,3}\s*,\s*){2}\d{1,3}(?:\s*,\s*(?:0(?:\.\d+)?|1(?:\.0+)?|0?\.\d+))?\s*\))$/i;

export async function synchronizeDesktopAppearance(appearance: DesktopAppearance): Promise<void> {
  if (!isDesktopRuntime()) return Promise.resolve();
  if (!DESKTOP_COLOR_PATTERN.test(appearance.backgroundColor)) {
    return Promise.reject(new Error('桌面壳背景色必须使用合法的十六进制或 rgba 颜色格式'));
  }
  if (!DESKTOP_COLOR_PATTERN.test(appearance.accentColor)) {
    return Promise.reject(new Error('桌面壳强调色必须使用合法的十六进制或 rgba 颜色格式'));
  }

  const desktop = window.magiDesktop;
  if (!desktop) throw new Error('desktop_preload_bridge_unavailable');
  await desktop.setAppearance(appearance);
}
