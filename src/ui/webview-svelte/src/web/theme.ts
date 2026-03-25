type WebThemeMode = 'light' | 'dark';

function resolveThemeMode(): WebThemeMode {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return 'dark';
  }
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
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

export function installWebTheme(): void {
  const media = typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-color-scheme: dark)')
    : null;

  applyThemeMode(resolveThemeMode());

  if (!media) {
    return;
  }

  const handleChange = (event: MediaQueryListEvent): void => {
    applyThemeMode(event.matches ? 'dark' : 'light');
  };

  if (typeof media.addEventListener === 'function') {
    media.addEventListener('change', handleChange);
    return;
  }

  media.addListener(handleChange);
}
