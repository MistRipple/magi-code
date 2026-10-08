<script lang="ts">
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { LOCALES, type LocaleCode } from '../i18n/locales';
  import { switchLocale } from '../lib/locale-switch';
  import { shellUi, togglePopover, closePopover } from '../stores/shell-ui.svelte';

  const open = $derived(shellUi.popover === 'language');
  let pendingLocale = $state<LocaleCode | null>(null);

  async function selectLocale(code: LocaleCode): Promise<void> {
    if (pendingLocale) return;
    if (code === i18n.locale) {
      closePopover('language');
      return;
    }
    pendingLocale = code;
    try {
      if (await switchLocale(code)) closePopover('language');
    } finally {
      pendingLocale = null;
    }
  }
</script>

<!-- 语言选择：图标按钮 + 向上展开的语言列表。语言来自 LOCALES 注册表，新增语言无需改这里。 -->
<div class="locale-switcher" data-shell-popover="language">
  <button
    type="button"
    class="btn-icon btn-icon--md"
    class:btn-icon--active={open}
    data-testid="sidebar-language"
    data-tooltip={open ? undefined : i18n.t('settings.locale.label')}
    data-tooltip-align="start"
    aria-label={i18n.t('settings.locale.label')}
    aria-haspopup="listbox"
    aria-expanded={open}
    onclick={() => togglePopover('language')}
  >
    <Icon name="globe" size={14} />
  </button>
  {#if open}
    <ul class="locale-menu" role="listbox" aria-label={i18n.t('settings.locale.label')}>
      {#each LOCALES as locale (locale.code)}
        <li role="presentation">
          <button
            type="button"
            class="locale-option"
            class:locale-option--active={locale.code === i18n.locale}
            role="option"
            aria-selected={locale.code === i18n.locale}
            lang={locale.code}
            disabled={pendingLocale !== null}
            data-testid={`locale-option-${locale.code}`}
            onclick={() => void selectLocale(locale.code)}
          >
            <span class="locale-option-name">{locale.nativeName}</span>
            {#if locale.code === i18n.locale}
              <Icon name="check" size={14} />
            {/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .locale-switcher {
    position: relative;
    display: flex;
  }

  .locale-menu {
    position: absolute;
    bottom: calc(100% + 8px);
    left: 0;
    min-width: 168px;
    max-height: min(60vh, 320px);
    overflow-y: auto;
    margin: 0;
    padding: var(--space-1);
    list-style: none;
    background: var(--glass-bg);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-xl);
    z-index: var(--z-popover);
    animation: localeMenuIn 0.15s ease-out;
  }

  @keyframes localeMenuIn {
    from { opacity: 0; transform: translateY(4px); }
    to { opacity: 1; transform: translateY(0); }
  }

  .locale-option {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    width: 100%;
    padding: var(--space-2) var(--space-3);
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .locale-option:hover:not(:disabled) {
    background: var(--surface-hover);
  }

  .locale-option:disabled {
    cursor: progress;
    opacity: 0.6;
  }

  .locale-option--active {
    font-weight: var(--font-medium);
    color: var(--primary);
  }
</style>
