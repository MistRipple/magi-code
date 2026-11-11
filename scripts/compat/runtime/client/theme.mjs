// Magi 外观到 DSH 公共 UI 语义的唯一映射；发行构建按随包 SDK 校验引用完整性。
export function dshThemeAliases(t) {
  // DSH 的层级色是完整颜色，也会被原包用作反色文字；Magi 的
  // surface-* 是用于叠加的透明材质，不能直接充当这些公共语义色。
  const canvas = t['--magi-canvas'] ?? t['--background'];
  const secondary = t['--foreground-secondary'];
  return {
    // secondary/tertiary 都可能承载可读正文与操作；沿共享设置页的文字层级，
    // 不把 Magi 的弱化色直接用于原包可点击页签。dimmed 保留弱化语义。
    '--dsw-alias-label-primary': t['--foreground'], '--dsw-alias-label-secondary': secondary,
    '--dsw-alias-label-tertiary': secondary, '--dsw-alias-label-dimmed': t['--foreground-muted'],
    '--dsw-alias-border-l1': t['--border-subtle'], '--dsw-alias-border-l2': t['--border'],
    '--dsw-alias-bg-base': t['--background'], '--dsw-alias-bg-layer-1': t['--background'],
    '--dsw-alias-bg-layer-2': t['--dropdown-bg'],
    '--dsw-alias-bg-layer-3': canvas,
    '--dsw-alias-bg-layer-4': t['--dropdown-bg'],
    '--dsw-alias-bg-l1': t['--surface-1'],
    '--dsw-alias-brand-primary': t['--primary'], '--dsw-alias-interactive-bg-hover': t['--surface-hover'],
    '--dsw-alias-link': t['--primary'],
    '--dsw-alias-state-success-primary': t['--success-text'], '--dsw-alias-state-warn-primary': t['--warning-text'],
    '--dsw-alias-state-error-primary': t['--error-text'], '--dsw-alias-label-error': t['--error-text'],
    '--dsw-alias-state-success-secondary': t['--success-text'], '--dsw-alias-state-success-tertiary': t['--success-bg'],
    '--dsw-alias-state-success-bg': t['--success-bg'],
    '--dsw-alias-state-warn-secondary': t['--warning-text'], '--dsw-alias-state-warn-tertiary': t['--warning-bg'],
    '--dsw-alias-state-warn-label': t['--warning-text'], '--dsw-alias-state-warn-bg': t['--warning-bg'],
    '--dsw-alias-state-error-secondary': t['--error-text'], '--dsw-alias-state-error-tertiary': t['--error-bg'],
    '--dsw-alias-state-error-bg': t['--error-bg'],
    '--dsw-alias-state-info-primary': t['--info-text'], '--dsw-alias-state-info-secondary': t['--info-text'],
    '--dsw-alias-state-info-tertiary': t['--info-bg'], '--dsw-alias-state-info-bg': t['--info-bg'],
    '--dsw-alias-state-idle-primary': t['--foreground-muted'],
    '--dsw-alias-state-business-primary': t['--primary'],
    '--dsw-alias-label-success': t['--success-text'], '--dsw-alias-label-warning': t['--warning-text'], '--dsw-alias-label-caution': t['--warning-text'],
    '--dsw-alias-label-primary-inverted': canvas,
    '--dsw-alias-label-shimmer': t['--foreground'],
    '--dsw-alias-label-primary-foreground': t['--btn-primary-fg'] ?? t['--primary-foreground'],
    '--dsw-alias-button-primary-fill': t['--btn-primary-bg'] ?? t['--primary'],
    '--dsw-alias-button-primary-hover': t['--btn-primary-hover-bg'] ?? t['--primary-hover'],
    '--dsw-alias-button-tool-bar-fill': t['--surface-2'], '--dsw-alias-button-tool-bar-hover': t['--surface-hover'],
    '--dsw-alias-button-ghost-active-fill': t['--surface-active'], '--dsw-alias-button-ghost-active-border': t['--border'],
    '--dsw-alias-interactive-bg-active': t['--surface-active'], '--dsw-alias-interactive-bg-hover-danger': t['--error-muted'],
    '--dsw-alias-bg-mask-1': t['--overlay'], '--dsw-alias-bg-skeleton': t['--surface-2'],
    '--dsw-alias-bg-module-platform': t['--dropdown-bg'], '--dsw-alias-border-l3': t['--border'], '--dsw-alias-border-l4': t['--border'],
    '--dsw-alias-border-l2-darkmode-thin': t['--border-subtle'],
    '--dsw-alias-switch-thumb': t['--primary-foreground'], '--dsw-alias-label-caption': t['--foreground-muted'],
    '--dsw-alias-tooltip-bg': t['--dropdown-bg'], '--dsw-alias-tooltip-key-bg': t['--surface-2'],
    '--dsw-alias-toast-bg': t['--dropdown-bg'], '--dsw-alias-toast-label': t['--foreground'],
    '--dsw-alias-menu-icon': t['--foreground'], '--dsw-alias-menu-group-header-fill': t['--surface-2'],
    '--dsw-alias-scrollbar-bg-l2': t['--scrollbar-thumb'], '--dsw-alias-scrollbar-hover-l2': t['--scrollbar-thumb-hover'],
    '--dsw-alias-markdown-code-block': t['--code-bg'], '--dsw-alias-markdown-code-block-banner': t['--surface-2'],
    '--dsw-alias-markdown-inline-code': t['--code-bg'],
    '--dsw-alias-code-diff-added': t['--success-bg'], '--dsw-alias-code-diff-deleted': t['--error-bg'],
    '--dsw-radius-xs': t['--radius-xs'], '--dsw-radius-sm': t['--radius-sm'], '--dsw-radius-md': t['--radius-md'],
    '--dsw-radius-lg': t['--radius-lg'], '--dsw-radius-xl': t['--radius-xl'], '--dsw-radius-panel': t['--radius-2xl'],
    '--dsw-elevation-soft': t['--shadow-sm'], '--dsw-elevation-panel': t['--shadow-sm'], '--dsw-elevation-prominent': t['--shadow-md'],
    '--dsw-shadow-lv3': t['--shadow-lg'], '--dsw-menu-surface-fill': t['--dropdown-bg'], '--dsw-hovercard-bg': t['--dropdown-bg'],
    '--dsw-focus-ring-color': t['--primary'], '--dsw-focus-ring-width': '2px', '--dsw-mask-blur': 'none',
    '--ds-transition-duration': t['--duration-normal'], '--ds-ease-in-out': t['--ease-in-out'],
    '--dsw-font-family': t['--vscode-font-family'], '--ds-font-family-code': t['--vscode-editor-font-family'],
    '--dsw-font-markdown-code-font-family': t['--vscode-editor-font-family'],
  };
}

// 只投影 Magi 已接纳的外观；不创建另一份主题注册表或持久偏好。
export function createThemeClient(request, reportError, onSuccess = () => {}) {
  const listeners = new Set();
  const publicId = id => ['builtin.light', 'builtin.dark', 'builtin.system'].includes(id) ? id.slice('builtin.'.length) : `magi:${id}`;
  let snapshot, identity, appearance, disposed = false;
  const freeze = value => {
    if (value && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value); }
    return value;
  };
  const service = {
    getTheme() { if (!snapshot) throw new Error('Magi 外观尚未就绪'); return snapshot; },
    setTheme(id) {
      if (disposed) throw new Error('插件页面已失效');
      const themeId = appearance?.themes.find(pack => publicId(pack.id) === id)?.id;
      if (typeof id !== 'string' || !themeId) throw new Error(`Magi 主题不存在：${id}`);
      // 上游写接口返回 void；异步提交失败由公共页面反馈展示，不发布乐观状态。
      void request({ operation: 'host.theme.set', themeId, expectedRevision: appearance.revision }).then(
        () => { if (!disposed) onSuccess(); }, error => { if (!disposed) reportError(error); });
    },
  };
  return {
    service,
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    update(theme) {
      if (disposed || !theme.appearance) return;
      const nextIdentity = JSON.stringify(theme);
      if (identity === nextIdentity) return;
      appearance = theme.appearance;
      const definition = pack => {
        const colorScheme = pack.schemePolicy === 'adaptive' ? theme.mode : pack.schemePolicy;
        const scheme = pack.schemes[colorScheme];
        return { id: publicId(pack.id), colorScheme, tokens: {
          '--dsw-alias-brand-primary': scheme.accent,
          '--dsw-alias-bg-base': scheme.background,
          '--dsw-alias-label-primary': scheme.foreground,
        } };
      };
      const activePack = appearance.activeTheme;
      snapshot = freeze({
        preference: publicId(appearance.preference),
        active: { ...definition(activePack), tokens: Object.fromEntries(Object.entries(dshThemeAliases(theme.tokens)).filter(([, value]) => value)) },
        themes: appearance.themes.filter(pack => pack.id !== 'builtin.system').map(definition),
        revision: (snapshot?.revision ?? 0) + 1,
      });
      identity = nextIdentity;
      for (const listener of [...listeners]) listener(snapshot);
    },
    dispose() { disposed = true; listeners.clear(); },
  };
}
