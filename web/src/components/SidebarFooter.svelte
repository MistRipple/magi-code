<script lang="ts">
  import Icon from './Icon.svelte';
  import DesktopUpdateStatus from './DesktopUpdateStatus.svelte';
  import type { IconName } from '../lib/icons';
  import { i18n } from '../stores/i18n.svelte';
  import { openSettings } from '../stores/shell-ui.svelte';

  interface Props {
    themeIcon: IconName;
    themeTitle: string;
    themeId: string;
    themeMode: string;
    onToggleTheme: () => void;
  }

  let { themeIcon, themeTitle, themeId, themeMode, onToggleTheme }: Props = $props();
</script>

<!-- 侧栏底部固定行：全局、低频的入口。位于滚动区之外，不随列表滚动。 -->
<div class="sidebar-footer">
  <div class="sidebar-footer-tools">
    <button
      type="button"
      class="btn-icon btn-icon--md"
      data-testid="sidebar-settings"
      data-tooltip={i18n.t('header.settings')}
      data-tooltip-align="start"
      aria-label={i18n.t('header.settings')}
      onclick={openSettings}
    >
      <Icon name="settings" size={14} />
    </button>
    <button
      type="button"
      class="btn-icon btn-icon--md"
      data-testid="sidebar-theme-toggle"
      data-tooltip={themeTitle}
      data-tooltip-align="start"
      data-theme-id={themeId}
      data-theme-mode={themeMode}
      aria-label={themeTitle}
      onclick={onToggleTheme}
    >
      <Icon name={themeIcon} size={14} />
    </button>
  </div>
  <DesktopUpdateStatus />
</div>

<style>
  .sidebar-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    flex-shrink: 0;
    /* 分隔线与侧栏底缘之间上下等距：内容居中。侧栏自身有底部内边距，
       用等量负外边距把它让出来，再由这里的 padding 重新分配。 */
    padding: var(--space-3) 0;
    margin-bottom: calc(var(--space-4) * -1);
    border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
  }

  .sidebar-footer-tools {
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }
</style>
