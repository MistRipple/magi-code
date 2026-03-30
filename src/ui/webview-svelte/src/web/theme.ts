export type WebThemeMode = 'light' | 'dark';
export type WebThemePreference = 'system' | WebThemeMode;

export interface WebThemeSnapshot {
  preference: WebThemePreference;
  mode: WebThemeMode;
}

const WEB_THEME_STORAGE_KEY = 'magi-web-theme-preference';
const webThemeListeners = new Set<(snapshot: WebThemeSnapshot) => void>();

let webThemePreference: WebThemePreference = 'system';
let activeWebThemeMode: WebThemeMode = 'dark';
let webThemeMediaQuery: MediaQueryList | null = null;
let webThemeMediaListenerAttached = false;

function parseRgbChannel(raw: string): number | null {
  const value = Number.parseFloat(raw.trim());
  if (!Number.isFinite(value)) {
    return null;
  }
  return Math.max(0, Math.min(255, value));
}

function parseHexColor(color: string): [number, number, number] | null {
  const hex = color.trim().replace(/^#/, '');
  if (![3, 6].includes(hex.length)) {
    return null;
  }

  const normalized = hex.length === 3
    ? hex.split('').map((char) => `${char}${char}`).join('')
    : hex;

  const r = Number.parseInt(normalized.slice(0, 2), 16);
  const g = Number.parseInt(normalized.slice(2, 4), 16);
  const b = Number.parseInt(normalized.slice(4, 6), 16);
  if ([r, g, b].some((value) => Number.isNaN(value))) {
    return null;
  }
  return [r, g, b];
}

function parseRgbColor(color: string): [number, number, number] | null {
  const match = color.trim().match(/^rgba?\((.+)\)$/i);
  if (!match) {
    return null;
  }
  const channels = match[1].split(',').slice(0, 3).map(parseRgbChannel);
  if (channels.length !== 3 || channels.some((value) => value === null)) {
    return null;
  }
  return channels as [number, number, number];
}

function parseColor(color: string): [number, number, number] | null {
  return parseHexColor(color) ?? parseRgbColor(color);
}

function relativeLuminance([r, g, b]: [number, number, number]): number {
  const normalize = (value: number): number => {
    const channel = value / 255;
    return channel <= 0.03928
      ? channel / 12.92
      : ((channel + 0.055) / 1.055) ** 2.4;
  };

  return (
    0.2126 * normalize(r)
    + 0.7152 * normalize(g)
    + 0.0722 * normalize(b)
  );
}

function readThemeVariable(names: string[]): string {
  if (typeof window === 'undefined') {
    return '';
  }

  const root = document.documentElement;
  const body = document.body;
  const targets = [body, root].filter(Boolean) as HTMLElement[];
  for (const target of targets) {
    const computed = window.getComputedStyle(target);
    for (const name of names) {
      const value = computed.getPropertyValue(name).trim();
      if (value) {
        return value;
      }
    }
  }
  return '';
}

function inferModeFromHostTheme(): WebThemeMode {
  if (typeof document === 'undefined') {
    return 'dark';
  }

  const root = document.documentElement;
  const body = document.body;
  const classNames = [
    ...(root ? Array.from(root.classList) : []),
    ...(body ? Array.from(body.classList) : []),
  ];

  if (classNames.includes('theme-light') || classNames.includes('vscode-light')) {
    return 'light';
  }
  if (classNames.includes('theme-dark') || classNames.includes('vscode-dark')) {
    return 'dark';
  }

  const background = readThemeVariable([
    '--vscode-sideBar-secondaryBackground',
    '--vscode-sideBar-background',
    '--vscode-editorWidget-background',
    '--vscode-editor-background',
    '--vscode-input-background',
  ]);
  const parsedBackground = parseColor(background);
  if (parsedBackground) {
    return relativeLuminance(parsedBackground) >= 0.45 ? 'light' : 'dark';
  }

  const foreground = readThemeVariable([
    '--vscode-editor-foreground',
    '--vscode-foreground',
  ]);
  const parsedForeground = parseColor(foreground);
  if (parsedForeground) {
    return relativeLuminance(parsedForeground) < 0.45 ? 'light' : 'dark';
  }

  return 'dark';
}

function resolveSystemThemeMode(): WebThemeMode {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return 'dark';
  }
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function resolveEffectiveWebThemeMode(preference: WebThemePreference = webThemePreference): WebThemeMode {
  return preference === 'system' ? resolveSystemThemeMode() : preference;
}

function readStoredWebThemePreference(): WebThemePreference {
  if (typeof window === 'undefined') {
    return 'system';
  }

  try {
    const raw = window.localStorage.getItem(WEB_THEME_STORAGE_KEY)?.trim();
    if (raw === 'light' || raw === 'dark' || raw === 'system') {
      return raw;
    }
  } catch (error) {
    console.warn('[theme] 读取 Web 主题偏好失败', error);
  }

  return 'system';
}

function persistWebThemePreference(preference: WebThemePreference): void {
  if (typeof window === 'undefined') {
    return;
  }

  try {
    window.localStorage.setItem(WEB_THEME_STORAGE_KEY, preference);
  } catch (error) {
    console.warn('[theme] 持久化 Web 主题偏好失败', error);
  }
}

function emitWebThemeSnapshot(): void {
  const snapshot = getWebThemeSnapshot();
  for (const listener of webThemeListeners) {
    listener(snapshot);
  }
}

function applyThemeMode(mode: WebThemeMode): void {
  const root = document.documentElement;
  const body = document.body;
  if (!root || !body) {
    return;
  }

  root.classList.remove('theme-light', 'theme-dark');
  root.classList.add(`theme-${mode}`);
  body.classList.remove('theme-light', 'theme-dark');
  body.classList.add(`theme-${mode}`);
}

function applyWebThemePreference(preference: WebThemePreference, persist = true): void {
  webThemePreference = preference;
  activeWebThemeMode = resolveEffectiveWebThemeMode(preference);
  applyThemeMode(activeWebThemeMode);
  if (persist) {
    persistWebThemePreference(preference);
  }
  emitWebThemeSnapshot();
}

export function installWebTheme(): void {
  if (typeof window === 'undefined') {
    return;
  }

  if (!webThemeMediaQuery && typeof window.matchMedia === 'function') {
    webThemeMediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
  }

  if (webThemeMediaQuery && !webThemeMediaListenerAttached) {
    const handleChange = (event: MediaQueryListEvent): void => {
      if (webThemePreference !== 'system') {
        return;
      }
      activeWebThemeMode = event.matches ? 'dark' : 'light';
      applyThemeMode(activeWebThemeMode);
      emitWebThemeSnapshot();
    };

    if (typeof webThemeMediaQuery.addEventListener === 'function') {
      webThemeMediaQuery.addEventListener('change', handleChange);
    } else {
      webThemeMediaQuery.addListener(handleChange);
    }
    webThemeMediaListenerAttached = true;
  }

  applyWebThemePreference(readStoredWebThemePreference(), false);
}

export function installHostTheme(): void {
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    return;
  }

  let framePending = false;
  const syncTheme = (): void => {
    if (framePending) {
      return;
    }
    framePending = true;
    window.requestAnimationFrame(() => {
      framePending = false;
      applyThemeMode(inferModeFromHostTheme());
    });
  };

  syncTheme();

  const observer = new MutationObserver(() => {
    syncTheme();
  });

  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['class', 'style', 'data-vscode-theme-id'],
  });

  if (document.body) {
    observer.observe(document.body, {
      attributes: true,
      attributeFilter: ['class', 'style', 'data-vscode-theme-id'],
    });
  }
}

export function getWebThemeSnapshot(): WebThemeSnapshot {
  return {
    preference: webThemePreference,
    mode: activeWebThemeMode,
  };
}

export function subscribeWebTheme(listener: (snapshot: WebThemeSnapshot) => void): () => void {
  webThemeListeners.add(listener);
  listener(getWebThemeSnapshot());
  return () => {
    webThemeListeners.delete(listener);
  };
}

export function setWebThemePreference(preference: WebThemePreference): void {
  applyWebThemePreference(preference);
}

export function cycleWebThemePreference(): WebThemePreference {
  const order: WebThemePreference[] = ['system', 'light', 'dark'];
  const currentIndex = order.indexOf(webThemePreference);
  const nextPreference = order[(currentIndex + 1) % order.length] || 'system';
  applyWebThemePreference(nextPreference);
  return nextPreference;
}
