import {
  activateAppearanceTheme,
  fetchAppearanceAsset,
  fetchAppearanceSnapshot,
} from './client';
import type { AppearanceSnapshot, ThemePack, ThemeScheme } from './contract';
import { synchronizeDesktopAppearance } from '../lib/desktop-appearance';

export type EffectiveAppearanceMode = 'light' | 'dark';

export interface AppearanceRuntimeSnapshot {
  library: AppearanceSnapshot | null;
  activeTheme: ThemePack | null;
  mode: EffectiveAppearanceMode;
  previewing: boolean;
}

const listeners = new Set<(snapshot: AppearanceRuntimeSnapshot) => void>();
const assetUrls = new Map<string, string>();
let library: AppearanceSnapshot | null = null;
let activeTheme: ThemePack | null = null;
let mode: EffectiveAppearanceMode = 'dark';
let previewing = false;
let applySequence = 0;
let mediaQuery: MediaQueryList | null = null;
let initialized = false;

export async function initializeAppearanceRuntime(): Promise<void> {
  if (initialized) return;
  initialized = true;
  try {
    window.localStorage.removeItem('magi-web-theme-preference');
  } catch {
    // 外观权威状态已迁移至 daemon，本地存储不可用不影响主题加载。
  }
  mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
  mediaQuery.addEventListener('change', handleSystemModeChange);
  window.addEventListener('magi:appearanceChanged', handleAppearanceChanged);
  try {
    await refreshAppearanceRuntime();
  } catch (error) {
    initialized = false;
    mediaQuery.removeEventListener('change', handleSystemModeChange);
    window.removeEventListener('magi:appearanceChanged', handleAppearanceChanged);
    mediaQuery = null;
    throw error;
  }
}

export async function refreshAppearanceRuntime(): Promise<void> {
  const snapshot = await fetchAppearanceSnapshot();
  await applyAppearanceSnapshot(snapshot);
}

export async function applyAppearanceSnapshot(snapshot: AppearanceSnapshot): Promise<void> {
  const record = snapshot.themes.find((candidate) => candidate.pack.id === snapshot.activeThemeId);
  if (!record) throw new Error('当前外观主题不存在');
  library = snapshot;
  previewing = false;
  await applyTheme(record.pack);
}

export async function previewAppearanceTheme(pack: ThemePack): Promise<void> {
  previewing = true;
  await applyTheme(pack);
}

export async function restoreActiveAppearance(): Promise<void> {
  if (!library) return;
  const record = library.themes.find((candidate) => candidate.pack.id === library?.activeThemeId);
  if (!record) return;
  previewing = false;
  await applyTheme(record.pack);
}

export function subscribeAppearanceRuntime(listener: (snapshot: AppearanceRuntimeSnapshot) => void): () => void {
  listeners.add(listener);
  listener(getAppearanceRuntimeSnapshot());
  return () => listeners.delete(listener);
}

export function getAppearanceRuntimeSnapshot(): AppearanceRuntimeSnapshot {
  return { library, activeTheme, mode, previewing };
}

export async function cycleBuiltinAppearance(): Promise<string | null> {
  if (!library) return null;
  const order = ['builtin.system', 'builtin.light', 'builtin.dark'];
  const index = order.indexOf(library.activeThemeId);
  const themeId = order[(index + 1) % order.length] || order[0];
  const snapshot = await activateAppearanceTheme(themeId, library.revision);
  await applyAppearanceSnapshot(snapshot);
  return themeId;
}

async function applyTheme(pack: ThemePack): Promise<void> {
  const sequence = ++applySequence;
  const nextMode = resolveMode(pack);
  const scheme = resolveScheme(pack, nextMode);
  const wallpaperUrl = pack.wallpaper ? await resolveAppearanceAssetUrl(pack.wallpaper.assetId) : '';
  if (sequence !== applySequence) {
    pruneAssetUrls(referencedAppearanceAssetIds(activeTheme?.wallpaper?.assetId));
    return;
  }
  activeTheme = pack;
  mode = nextMode;
  applyRootTheme(pack, scheme, wallpaperUrl);
  pruneAssetUrls(referencedAppearanceAssetIds(pack.wallpaper?.assetId));
  emit();
  void synchronizeDesktopAppearance({
    backgroundColor: scheme.background,
    mode: nextMode,
  }).catch((error) => {
    console.error('[appearance] 同步桌面壳外观失败', error);
  });
}

function resolveMode(pack: ThemePack): EffectiveAppearanceMode {
  if (pack.schemePolicy === 'light' || pack.schemePolicy === 'dark') return pack.schemePolicy;
  return mediaQuery?.matches ? 'dark' : 'light';
}

function resolveScheme(pack: ThemePack, targetMode: EffectiveAppearanceMode): ThemeScheme {
  const scheme = pack.schemes[targetMode];
  if (!scheme) throw new Error(`主题缺少${targetMode === 'light' ? '浅色' : '深色'}方案`);
  return scheme;
}

export async function resolveAppearanceAssetUrl(assetId: string): Promise<string> {
  const cached = assetUrls.get(assetId);
  if (cached) return cached;
  const payload = await fetchAppearanceAsset(assetId);
  const binary = atob(payload.dataBase64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  const url = URL.createObjectURL(new Blob([bytes], { type: payload.mimeType }));
  assetUrls.set(assetId, url);
  return url;
}

function referencedAppearanceAssetIds(extraAssetId?: string): Set<string> {
  const retained = new Set(
    library?.themes.flatMap((record) => record.pack.wallpaper?.assetId ?? []) ?? [],
  );
  if (extraAssetId) retained.add(extraAssetId);
  return retained;
}

function pruneAssetUrls(retainedAssetIds: ReadonlySet<string>): void {
  for (const [assetId, url] of assetUrls) {
    if (retainedAssetIds.has(assetId)) continue;
    URL.revokeObjectURL(url);
    assetUrls.delete(assetId);
  }
}

function applyRootTheme(pack: ThemePack, scheme: ThemeScheme, wallpaperUrl: string): void {
  const root = document.documentElement;
  const body = document.body;
  root.classList.remove('theme-light', 'theme-dark');
  body.classList.remove('theme-light', 'theme-dark');
  root.classList.add(`theme-${mode}`);
  body.classList.add(`theme-${mode}`);
  root.dataset.magiThemeId = pack.id;
  root.dataset.magiMaterial = pack.material;
  root.dataset.magiScheme = mode;

  const background = parseHex(scheme.background);
  const foreground = parseHex(scheme.foreground);
  const accent = parseHex(scheme.accent);
  const muted = mix(background, foreground, mode === 'dark' ? 0.62 : 0.58);
  const contrastFactor = Math.max(0.2, Math.min(0.9, scheme.contrast / 100));
  const materialAlpha = pack.material === 'clear' ? 0.98 : pack.material === 'translucent' ? 0.84 : 0.7;
  const panel = rgba(background, wallpaperUrl ? materialAlpha : 1);
  const elevated = rgba(background, wallpaperUrl ? Math.min(0.98, materialAlpha + 0.1) : 1);
  const critical = rgba(background, wallpaperUrl ? 0.96 : 1);
  const windowOverlay = rgba(background, wallpaperUrl
    ? pack.material === 'clear' ? 0.96 : pack.material === 'translucent' ? 0.78 : 0.68
    : 1);
  const popoverOverlay = rgba(background, wallpaperUrl
    ? pack.material === 'clear' ? 0.98 : pack.material === 'translucent' ? 0.9 : 0.84
    : 1);
  const criticalOverlay = rgba(background, wallpaperUrl ? 0.97 : 1);
  const primaryForeground = relativeLuminance(accent) > 0.44 ? '#111827' : '#FFFFFF';

  const variables: Record<string, string> = {
    '--background': panel,
    '--foreground': scheme.foreground,
    '--foreground-rgb': foreground.join(', '),
    '--foreground-muted': toHex(muted),
    '--border': rgba(foreground, 0.14 + contrastFactor * 0.12),
    '--border-subtle': rgba(foreground, 0.06 + contrastFactor * 0.06),
    '--primary': scheme.accent,
    '--primary-rgb': accent.join(', '),
    '--primary-hover': toHex(mix(accent, foreground, 0.18)),
    '--primary-muted': rgba(accent, 0.15),
    '--primary-foreground': primaryForeground,
    '--secondary': rgba(foreground, 0.08),
    '--secondary-hover': rgba(foreground, 0.13),
    '--surface': elevated,
    '--surface-1': rgba(foreground, 0.02 + contrastFactor * 0.015),
    '--surface-2': rgba(foreground, 0.04 + contrastFactor * 0.025),
    '--surface-3': rgba(foreground, 0.06 + contrastFactor * 0.035),
    '--surface-hover': rgba(foreground, 0.07 + contrastFactor * 0.035),
    '--surface-active': rgba(foreground, 0.1 + contrastFactor * 0.04),
    '--surface-selected': rgba(accent, 0.15),
    '--dropdown-bg': elevated,
    '--glass-bg': elevated,
    '--code-bg': critical,
    '--code-border': rgba(foreground, 0.14 + contrastFactor * 0.12),
    '--user-message-bg': rgba(accent, 0.16),
    '--assistant-message-bg': panel,
    '--overlay': mode === 'dark' ? 'rgba(0, 0, 0, 0.58)' : 'rgba(15, 23, 42, 0.34)',
    '--overlay-heavy': mode === 'dark' ? 'rgba(0, 0, 0, 0.76)' : 'rgba(15, 23, 42, 0.52)',
    '--scrollbar-thumb': rgba(foreground, 0.16),
    '--scrollbar-thumb-hover': rgba(foreground, 0.28),
    '--magi-canvas': scheme.background,
    '--magi-surface-sidebar': panel,
    '--magi-surface-main': panel,
    '--magi-surface-right-pane': panel,
    '--magi-surface-dialog': elevated,
    '--magi-surface-popover': elevated,
    '--magi-surface-critical': critical,
    '--magi-window-overlay': windowOverlay,
    '--magi-popover-overlay': popoverOverlay,
    '--magi-critical-overlay': criticalOverlay,
    '--magi-wallpaper-image': wallpaperUrl ? `url("${wallpaperUrl}")` : 'none',
    '--magi-wallpaper-position': `${(pack.wallpaper?.focusX ?? 0.5) * 100}% ${(pack.wallpaper?.focusY ?? 0.5) * 100}%`,
    '--magi-wallpaper-dim': String(pack.wallpaper?.dim ?? 0),
    '--magi-wallpaper-blur': `${pack.wallpaper?.blur ?? 0}px`,
    '--vscode-sideBar-background': panel,
    '--vscode-sideBar-secondaryBackground': panel,
    '--vscode-editor-background': panel,
    '--vscode-editor-background-rgb': background.join(', '),
    '--vscode-editor-foreground': scheme.foreground,
    '--vscode-foreground': scheme.foreground,
    '--vscode-descriptionForeground': toHex(muted),
    '--vscode-panel-border': rgba(foreground, 0.14 + contrastFactor * 0.12),
    '--vscode-widget-border': rgba(foreground, 0.14 + contrastFactor * 0.12),
    '--vscode-editorWidget-background': elevated,
    '--vscode-editorWidget-border': rgba(foreground, 0.14 + contrastFactor * 0.12),
    '--vscode-dropdown-background': elevated,
    '--vscode-input-background': elevated,
    '--vscode-input-border': rgba(foreground, 0.14 + contrastFactor * 0.12),
    '--vscode-textCodeBlock-background': critical,
    '--vscode-textLink-foreground': scheme.accent,
    '--vscode-focusBorder': scheme.accent,
    '--vscode-progressBar-background': scheme.accent,
    '--vscode-button-background': scheme.accent,
    '--vscode-button-hoverBackground': toHex(mix(accent, foreground, 0.18)),
    '--vscode-button-foreground': primaryForeground,
    '--vscode-button-secondaryBackground': rgba(foreground, 0.08),
    '--vscode-button-secondaryHoverBackground': rgba(foreground, 0.13),
    '--vscode-list-hoverBackground': rgba(foreground, 0.07),
    '--vscode-list-activeSelectionBackground': rgba(accent, 0.18),
    '--vscode-list-activeSelectionForeground': scheme.foreground,
    '--vscode-inputOption-activeBackground': rgba(accent, 0.18),
  };
  for (const [name, value] of Object.entries(variables)) root.style.setProperty(name, value);
}

function parseHex(value: string): [number, number, number] {
  const normalized = value.replace('#', '');
  return [0, 2, 4].map((offset) => Number.parseInt(normalized.slice(offset, offset + 2), 16)) as [number, number, number];
}

function mix(left: [number, number, number], right: [number, number, number], amount: number): [number, number, number] {
  return left.map((value, index) => Math.round(value * (1 - amount) + right[index] * amount)) as [number, number, number];
}

function toHex(color: [number, number, number]): string {
  return `#${color.map((value) => value.toString(16).padStart(2, '0')).join('')}`;
}

function rgba(color: [number, number, number], alpha: number): string {
  return `rgba(${color[0]}, ${color[1]}, ${color[2]}, ${alpha.toFixed(3)})`;
}

function relativeLuminance(color: [number, number, number]): number {
  const [red, green, blue] = color.map((value) => {
    const channel = value / 255;
    return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

function handleSystemModeChange(): void {
  if (activeTheme?.schemePolicy === 'adaptive') void applyTheme(activeTheme);
}

function handleAppearanceChanged(): void {
  void refreshAppearanceRuntime().catch((error) => {
    console.error('[appearance] 同步外观状态失败', error);
  });
}

function emit(): void {
  const snapshot = getAppearanceRuntimeSnapshot();
  for (const listener of listeners) listener(snapshot);
}
