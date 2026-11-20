// Projects Magi's live theme tokens onto DSH's `--dsw-*` semantic CSS variables.

export function dshThemeAliases(t = {}) {
  const canvas = t['--magi-canvas'] || t['--background'] || '#141416';
  const secondary = t['--foreground-secondary'] || t['--foreground'] || '#a1a1aa';
  return {
    '--dsw-alias-label-primary': t['--foreground'],
    '--dsw-alias-label-secondary': secondary,
    '--dsw-alias-label-tertiary': secondary,
    '--dsw-alias-label-dimmed': t['--foreground-muted'],
    '--dsw-alias-border-l1': t['--border-subtle'],
    '--dsw-alias-border-l2': t['--border'],
    '--dsw-alias-border-l3': t['--border'],
    '--dsw-alias-border-l4': t['--border'],
    '--dsw-alias-border-inverted': t['--border'],
    '--dsw-alias-border-l2-darkmode-thin': t['--border-subtle'],
    '--dsw-alias-bg-base': t['--background'],
    '--dsw-alias-bg-layer-1': t['--background'],
    '--dsw-alias-bg-layer-2': t['--dropdown-bg'] || t['--surface-2'],
    '--dsw-alias-bg-layer-3': canvas,
    '--dsw-alias-bg-layer-4': t['--dropdown-bg'] || t['--surface-2'],
    '--dsw-alias-bg-l1': t['--surface-1'],
    '--dsw-alias-brand-primary': t['--primary'],
    '--dsw-alias-interactive-bg-hover': t['--surface-hover'],
    '--dsw-alias-interactive-bg-active': t['--surface-active'],
    '--dsw-alias-interactive-bg-hover-danger': t['--error-muted'],
    '--dsw-alias-link': t['--primary'],
    '--dsw-alias-state-success-primary': t['--success-text'] || t['--success'],
    '--dsw-alias-state-success-secondary': t['--success-text'] || t['--success'],
    '--dsw-alias-state-success-tertiary': t['--success-bg'],
    '--dsw-alias-state-success-bg': t['--success-bg'],
    '--dsw-alias-state-warn-primary': t['--warning-text'] || t['--warning'],
    '--dsw-alias-state-warn-secondary': t['--warning-text'] || t['--warning'],
    '--dsw-alias-state-warn-tertiary': t['--warning-bg'],
    '--dsw-alias-state-warn-label': t['--warning-text'] || t['--warning'],
    '--dsw-alias-state-warn-bg': t['--warning-bg'],
    '--dsw-alias-state-error-primary': t['--error-text'] || t['--error'],
    '--dsw-alias-state-error-secondary': t['--error-text'] || t['--error'],
    '--dsw-alias-state-error-tertiary': t['--error-bg'],
    '--dsw-alias-state-error-bg': t['--error-bg'],
    '--dsw-alias-state-info-primary': t['--info-text'] || t['--info'],
    '--dsw-alias-state-info-secondary': t['--info-text'] || t['--info'],
    '--dsw-alias-state-info-tertiary': t['--info-bg'],
    '--dsw-alias-state-info-bg': t['--info-bg'],
    '--dsw-alias-state-idle-primary': t['--foreground-muted'],
    '--dsw-alias-state-business-primary': t['--primary'],
    '--dsw-alias-label-error': t['--error-text'] || t['--error'],
    '--dsw-alias-label-success': t['--success-text'] || t['--success'],
    '--dsw-alias-label-warning': t['--warning-text'] || t['--warning'],
    '--dsw-alias-label-caution': t['--warning-text'] || t['--warning'],
    '--dsw-alias-label-primary-inverted': canvas,
    '--dsw-alias-label-shimmer': t['--foreground'],
    '--dsw-alias-label-primary-foreground': t['--btn-primary-fg'] || t['--primary-foreground'],
    '--dsw-alias-button-primary-fill': t['--btn-primary-bg'] || t['--primary'],
    '--dsw-alias-button-primary-hover': t['--btn-primary-hover-bg'] || t['--primary-hover'],
    '--dsw-alias-button-tool-bar-fill': t['--surface-2'],
    '--dsw-alias-button-tool-bar-hover': t['--surface-hover'],
    '--dsw-alias-button-ghost-active-fill': t['--surface-active'],
    '--dsw-alias-button-ghost-active-border': t['--border'],
    '--dsw-alias-bg-mask-1': t['--overlay'],
    '--dsw-alias-bg-skeleton': t['--surface-2'],
    '--dsw-alias-bg-module-platform': t['--dropdown-bg'] || t['--surface-2'],
    '--dsw-alias-switch-thumb': t['--primary-foreground'] || '#ffffff',
    '--dsw-alias-label-caption': t['--foreground-muted'],
    '--dsw-alias-tooltip-bg': t['--dropdown-bg'] || t['--surface-2'],
    '--dsw-alias-tooltip-key-bg': t['--surface-2'],
    '--dsw-alias-toast-bg': t['--dropdown-bg'] || t['--surface-2'],
    '--dsw-alias-toast-label': t['--foreground'],
    '--dsw-alias-menu-icon': t['--foreground'],
    '--dsw-alias-menu-group-header-fill': t['--surface-2'],
    '--dsw-alias-scrollbar-bg-l2': t['--scrollbar-thumb'],
    '--dsw-alias-scrollbar-hover-l2': t['--scrollbar-thumb-hover'],
    '--dsw-alias-markdown-code-block': t['--code-bg'],
    '--dsw-alias-markdown-code-block-banner': t['--surface-2'],
    '--dsw-alias-markdown-inline-code': t['--code-bg'],
    '--dsw-alias-code-diff-added': t['--success-bg'],
    '--dsw-alias-code-diff-deleted': t['--error-bg'],
    '--dsw-radius-xs': t['--radius-xs'] || '4px',
    '--dsw-radius-sm': t['--radius-sm'] || '6px',
    '--dsw-radius-md': t['--radius-md'] || '8px',
    '--dsw-radius-lg': t['--radius-lg'] || '10px',
    '--dsw-radius-xl': t['--radius-xl'] || '12px',
    '--dsw-radius-panel': t['--radius-2xl'] || '16px',
    '--dsw-elevation-soft': t['--shadow-sm'],
    '--dsw-elevation-panel': t['--shadow-sm'],
    '--dsw-elevation-prominent': t['--shadow-md'],
    '--dsw-shadow-lv3': t['--shadow-lg'],
    '--dsw-menu-surface-fill': t['--dropdown-bg'] || t['--surface-2'],
    '--dsw-hovercard-bg': t['--dropdown-bg'] || t['--surface-2'],
    '--dsw-focus-ring-color': t['--primary'],
    '--dsw-focus-ring-width': '2px',
    '--dsw-mask-blur': 'none',
    '--ds-transition-duration': t['--duration-normal'] || '150ms',
    '--ds-ease-in-out': t['--ease-in-out'] || 'ease-in-out',
    '--dsw-font-family': t['--vscode-font-family'] || 'system-ui, -apple-system, sans-serif',
    '--ds-font-family-code': t['--vscode-editor-font-family'] || 'ui-monospace, SFMono-Regular, Menlo, monospace',
    '--dsw-font-markdown-code-font-family': t['--vscode-editor-font-family'] || 'ui-monospace, SFMono-Regular, Menlo, monospace',
  };
}

export function applyThemeToDocument(theme) {
  if (!theme || typeof document === 'undefined') return;
  const root = document.documentElement;
  const mode = theme.mode === 'light' ? 'light' : 'dark';
  root.dataset.theme = mode;
  root.style.colorScheme = mode;
  root.classList.toggle('theme-light', mode === 'light');
  root.classList.toggle('theme-dark', mode !== 'light');
  const tokens = theme.tokens ?? {};
  for (const [key, value] of Object.entries(tokens)) {
    if (typeof value === 'string' && value) root.style.setProperty(key, value);
  }
  for (const [key, value] of Object.entries(dshThemeAliases(tokens))) {
    if (typeof value === 'string' && value) root.style.setProperty(key, value);
  }
}
