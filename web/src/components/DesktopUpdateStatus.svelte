<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { isDesktopRuntime } from '../lib/desktop-updater';
  import {
    checkForDesktopUpdate,
    desktopUpdaterState,
    downloadDesktopUpdate,
    restartWithDesktopUpdate,
    retryDesktopUpdate,
    startDesktopUpdater,
  } from '../stores/desktop-updater.svelte';

  const desktopRuntime = isDesktopRuntime();
  const currentVersion = $derived(desktopUpdaterState.currentVersion);
  const update = $derived(desktopUpdaterState.update);
  const phase = $derived(desktopUpdaterState.phase);
  const progress = $derived(desktopUpdaterState.progress);
  const error = $derived(desktopUpdaterState.error);

  const actionTone = $derived.by(() => {
    if (phase === 'available') return 'available';
    if (phase === 'downloading') return 'downloading';
    if (phase === 'ready') return 'ready';
    if (phase === 'installing') return 'installing';
    if (phase === 'checking') return 'checking';
    if (phase === 'error') return 'error';
    return 'idle';
  });

  const actionLabel = $derived.by(() => {
    if (phase === 'available') return `v${update?.version || currentVersion}`;
    if (phase === 'downloading') return progress?.percent !== undefined ? `${progress.percent}%` : '…';
    if (phase === 'ready') return i18n.t('app.update.restartNow');
    if (phase === 'installing') return i18n.t('app.update.restarting');
    if (phase === 'checking') return i18n.t('settings.update.checking');
    if (phase === 'error') return i18n.t('settings.update.retry');
    return i18n.t('settings.update.check');
  });

  const actionTitle = $derived.by(() => {
    if (phase === 'error') return error || i18n.t('app.update.retryHint');
    if (phase === 'available') {
      return i18n.t('settings.update.available', { version: update?.version || '' });
    }
    if (phase === 'downloading' && progress?.percent === undefined) {
      return i18n.t('app.update.progressUnknown');
    }
    if (phase === 'ready') return i18n.t('app.update.readyHint');
    if (phase === 'checking') return i18n.t('settings.update.checking');
    return i18n.t('settings.update.check');
  });

  const actionIcon = $derived(
    phase === 'error' ? 'warning' : phase === 'available' || phase === 'downloading' ? 'download' : 'refresh',
  );
  const showActionLabel = $derived(
    phase === 'available' || phase === 'downloading' || phase === 'ready' || phase === 'installing',
  );
  const actionDisabled = $derived(phase === 'checking' || phase === 'downloading' || phase === 'installing');

  function activateAction(): void {
    if (actionDisabled) return;
    if (phase === 'available') {
      void downloadDesktopUpdate();
      return;
    }
    if (phase === 'ready') {
      void restartWithDesktopUpdate();
      return;
    }
    if (phase === 'error') {
      void retryDesktopUpdate();
      return;
    }
    void checkForDesktopUpdate('manual');
  }

  onMount(startDesktopUpdater);
</script>

{#if desktopRuntime && currentVersion}
  <div class="header-update-status" data-update-phase={phase} aria-live="polite">
    <span class="header-update-version">v{currentVersion}</span>
    <button
      type="button"
      class={`btn-icon header-action-btn header-update-action header-update-action--${actionTone}`}
      class:header-update-action--with-label={showActionLabel}
      aria-label={actionTitle}
      title={actionTitle}
      aria-busy={phase === 'checking' || phase === 'downloading' || phase === 'installing'}
      disabled={actionDisabled}
      onclick={activateAction}
    >
      <Icon name={actionIcon} size={12} />
      {#if showActionLabel}
        <span class="header-update-action-label">{actionLabel}</span>
      {/if}
    </button>
  </div>
{/if}

<style>
  .header-update-status {
    display: inline-flex;
    align-items: center;
    gap: 1px;
    flex: 0 0 auto;
    min-width: 0;
  }

  .header-update-version {
    flex: 0 0 auto;
    height: 32px;
    padding: 0 4px;
    display: inline-flex;
    align-items: center;
    color: var(--foreground-muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .header-update-action {
    width: 32px;
    height: 32px;
    flex: 0 0 32px;
    padding: 0;
    gap: 5px;
    color: var(--update-tone);
    transition: background var(--transition-fast), color var(--transition-fast), opacity var(--transition-fast);
  }

  .header-update-action--idle {
    --update-tone: var(--foreground-muted);
  }

  .header-update-action--checking {
    --update-tone: var(--primary);
  }

  .header-update-action--available {
    --update-tone: var(--warning, #d97706);
  }

  .header-update-action--downloading {
    --update-tone: var(--color-codex, var(--info, #3b82f6));
  }

  .header-update-action--ready {
    --update-tone: var(--success, #16a34a);
  }

  .header-update-action--installing {
    --update-tone: var(--color-orchestrator, #8b5cf6);
  }

  .header-update-action--error {
    --update-tone: var(--error, #dc2626);
  }

  .header-update-action--with-label {
    width: auto;
    min-width: 0;
    flex-basis: auto;
    padding: 0 8px;
    font-size: 11px;
    font-weight: var(--font-semibold);
    font-variant-numeric: tabular-nums;
    line-height: 1;
    white-space: nowrap;
  }

  .header-update-action:hover:not(:disabled) {
    background: var(--surface-hover);
  }

  .header-update-action--idle:hover:not(:disabled) {
    color: var(--foreground);
  }

  .header-update-action:disabled {
    cursor: wait;
    opacity: 0.86;
  }

  @media (max-width: 768px) {
    .header-update-status {
      gap: 1px;
    }

    .header-update-action--with-label {
      padding: 0 6px;
    }
  }
</style>
