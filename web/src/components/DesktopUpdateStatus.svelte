<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
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
  import type { DesktopUpdateCheckResult } from '../stores/desktop-updater.svelte';
  import { showFeedback } from '../lib/notifications';

  const desktopRuntime = isDesktopRuntime();
  const currentVersion = $derived(desktopUpdaterState.currentVersion);
  const update = $derived(desktopUpdaterState.update);
  const phase = $derived(desktopUpdaterState.phase);
  const progress = $derived(desktopUpdaterState.progress);
  const error = $derived(desktopUpdaterState.error);

  interface ActionPresentation {
    tone: 'idle' | 'checking' | 'latest' | 'available' | 'downloading' | 'ready' | 'installing' | 'error';
    icon: IconName;
    label: string;
    expanded: boolean;
    spinning: boolean;
    progress: boolean;
  }

  const actionPresentation = $derived.by((): ActionPresentation => {
    if (update && !update.installability.installable) {
      return {
        tone: 'error',
        icon: 'warning',
        label: i18n.t('app.update.installFirst'),
        expanded: true,
        spinning: false,
        progress: false,
      };
    }

    switch (phase) {
      case 'checking':
        return {
          tone: 'checking',
          icon: 'refresh',
          label: '',
          expanded: false,
          spinning: true,
          progress: false,
        };
      case 'latest':
        return {
          tone: 'latest',
          icon: 'check-circle',
          label: '',
          expanded: false,
          spinning: false,
          progress: false,
        };
      case 'available':
        return {
          tone: 'available',
          icon: 'download',
          label: i18n.t('app.update.update'),
          expanded: true,
          spinning: false,
          progress: false,
        };
      case 'downloading':
        return {
          tone: 'downloading',
          icon: 'download',
          label: progress?.percent === undefined ? '…' : `${progress.percent}%`,
          expanded: true,
          spinning: false,
          progress: true,
        };
      case 'ready':
        return {
          tone: 'ready',
          icon: 'restart',
          label: i18n.t('app.update.restart'),
          expanded: true,
          spinning: false,
          progress: false,
        };
      case 'installing':
        return {
          tone: 'installing',
          icon: 'restart',
          label: '',
          expanded: false,
          spinning: true,
          progress: false,
        };
      case 'error':
        return {
          tone: 'error',
          icon: 'warning',
          label: i18n.t('app.update.retry'),
          expanded: true,
          spinning: false,
          progress: false,
        };
      default:
        return {
          tone: 'idle',
          icon: 'refresh',
          label: '',
          expanded: false,
          spinning: false,
          progress: false,
        };
    }
  });

  const actionTitle = $derived.by(() => {
    if (phase === 'error') return error || i18n.t('app.update.retryHint');
    if (update && !update.installability.installable) {
      return i18n.t('app.update.installationRequiredHint');
    }
    if (phase === 'available') {
      return i18n.t('settings.update.available', { version: update?.version || '' });
    }
    if (phase === 'downloading') {
      return progress?.percent === undefined
        ? i18n.t('app.update.progressUnknown')
        : i18n.t('app.update.downloadingProgress', { percent: progress.percent });
    }
    if (phase === 'ready') return i18n.t('app.update.readyHint');
    if (phase === 'installing') return i18n.t('app.update.restarting');
    if (phase === 'checking') return i18n.t('settings.update.checking');
    if (phase === 'latest') return i18n.t('app.update.latestHint', { version: currentVersion });
    return i18n.t('settings.update.check');
  });

  const actionDisabled = $derived(phase === 'checking' || phase === 'downloading' || phase === 'installing');
  const downloadProgress = $derived(progress?.percent ?? 0);
  let announcedUpdateVersion = $state('');

  function showCheckFeedback(result: DesktopUpdateCheckResult): void {
    if (result === 'latest') {
      showFeedback('success', i18n.t('app.update.latestMessage', { version: currentVersion }), {
        title: i18n.t('app.update.latestTitle'),
        source: 'desktop-update',
        duration: 5_000,
        presentation: 'toast',
      });
    }
  }

  function showInstallationRequiredFeedback(): void {
    showFeedback('warning', i18n.t('app.update.installationRequiredMessage'), {
      title: i18n.t('app.update.installationRequiredTitle'),
      source: 'desktop-update',
      duration: 7_000,
      presentation: 'toast',
    });
  }

  $effect(() => {
    const availableUpdate = phase === 'available' ? update : null;
    const availableVersion = availableUpdate?.version || '';
    if (!availableVersion || announcedUpdateVersion === availableVersion) return;
    announcedUpdateVersion = availableVersion;
    const installationRequired = !availableUpdate?.installability.installable;
    showFeedback(
      installationRequired ? 'warning' : 'info',
      installationRequired
        ? i18n.t('app.update.installationRequiredMessage')
        : i18n.t('app.update.availableMessage', { version: availableVersion }),
      {
        title: installationRequired
          ? i18n.t('app.update.installationRequiredTitle')
          : i18n.t('app.update.availableTitle'),
        source: 'desktop-update',
        duration: 5_000,
        presentation: 'toast',
      },
    );
  });

  async function activateAction(): Promise<void> {
    if (actionDisabled) return;
    if (update && !update.installability.installable) {
      showInstallationRequiredFeedback();
      return;
    }
    if (phase === 'available') {
      await downloadDesktopUpdate();
      return;
    }
    if (phase === 'ready') {
      await restartWithDesktopUpdate();
      return;
    }
    if (phase === 'error') {
      showCheckFeedback(await retryDesktopUpdate());
      return;
    }
    showCheckFeedback(await checkForDesktopUpdate('manual'));
  }

  onMount(startDesktopUpdater);
</script>

{#if desktopRuntime && currentVersion}
  <div class="header-update-status" data-update-phase={phase} aria-live="polite">
    <span class="header-update-action-slot">
      <button
        type="button"
        class={`header-update-action header-update-action--${actionPresentation.tone}`}
        class:header-update-action--expanded={actionPresentation.expanded}
        aria-label={actionTitle}
        title={actionTitle}
        aria-busy={phase === 'checking' || phase === 'downloading' || phase === 'installing'}
        aria-disabled={actionDisabled}
        onclick={activateAction}
      >
        {#if actionPresentation.progress}
          <svg
            class="header-update-progress-ring"
            class:header-update-progress-ring--indeterminate={progress?.percent === undefined}
            viewBox="0 0 20 20"
            aria-hidden="true"
          >
            <circle class="header-update-progress-track" cx="10" cy="10" r="8" pathLength="100"></circle>
            <circle
              class="header-update-progress-value"
              cx="10"
              cy="10"
              r="8"
              pathLength="100"
              style:stroke-dashoffset={100 - downloadProgress}
            ></circle>
          </svg>
        {:else}
          <Icon
            name={actionPresentation.icon}
            size={14}
            class={`header-update-action-icon${actionPresentation.spinning ? ' header-update-action-icon--spinning' : ''}`}
          />
        {/if}
        {#if actionPresentation.label}
          <span class="header-update-action-label">{actionPresentation.label}</span>
        {/if}
      </button>
    </span>
    <span class="header-update-version">v{currentVersion}</span>
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

  .header-update-action-slot {
    display: inline-flex;
    width: 70px;
    height: 32px;
    align-items: center;
    justify-content: flex-end;
    flex: 0 0 70px;
  }

  .header-update-action {
    position: relative;
    display: inline-flex;
    width: 32px;
    height: 32px;
    align-items: center;
    justify-content: center;
    flex: 0 0 32px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    gap: 4px;
    color: var(--update-tone);
    cursor: pointer;
    transition: width var(--transition-fast), background var(--transition-fast), color var(--transition-fast), opacity var(--transition-fast);
  }

  .header-update-action--expanded {
    width: 70px;
    flex-basis: 70px;
    padding: 0 7px;
  }

  .header-update-action--idle {
    --update-tone: var(--foreground-muted);
  }

  .header-update-action--checking {
    --update-tone: var(--primary);
  }

  .header-update-action--latest {
    --update-tone: var(--success, #16a34a);
  }

  .header-update-action--available {
    --update-tone: var(--warning, #d97706);
    background: color-mix(in srgb, var(--warning, #d97706) 11%, transparent);
  }

  .header-update-action--downloading {
    --update-tone: var(--color-codex, var(--info, #3b82f6));
    background: color-mix(in srgb, var(--color-codex, var(--info, #3b82f6)) 10%, transparent);
  }

  .header-update-action--ready {
    --update-tone: var(--success, #16a34a);
    background: color-mix(in srgb, var(--success, #16a34a) 14%, transparent);
  }

  .header-update-action--installing {
    --update-tone: var(--color-orchestrator, #8b5cf6);
    background: color-mix(in srgb, var(--color-orchestrator, #8b5cf6) 10%, transparent);
  }

  .header-update-action--error {
    --update-tone: var(--error, #dc2626);
    background: color-mix(in srgb, var(--error, #dc2626) 9%, transparent);
  }

  .header-update-action:hover:not([aria-disabled='true']) {
    background: var(--surface-hover);
  }

  .header-update-action--idle:hover:not([aria-disabled='true']) {
    color: var(--foreground);
  }

  .header-update-action[aria-disabled='true'] {
    cursor: default;
    opacity: 0.86;
  }

  :global(.header-update-action-icon) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transform-origin: center;
  }

  :global(.header-update-action-icon--spinning) {
    will-change: transform;
    animation: header-update-action-spin 0.8s linear infinite;
  }

  .header-update-action-label {
    min-width: 0;
    color: currentColor;
    font-size: 11px;
    font-weight: var(--font-semibold);
    font-variant-numeric: tabular-nums;
    line-height: 1;
    white-space: nowrap;
  }

  .header-update-progress-ring {
    width: 18px;
    height: 18px;
    flex: 0 0 18px;
    overflow: visible;
    transform: rotate(-90deg);
  }

  .header-update-progress-track,
  .header-update-progress-value {
    fill: none;
    stroke-width: 2;
  }

  .header-update-progress-track {
    stroke: color-mix(in srgb, currentColor 20%, transparent);
  }

  .header-update-progress-value {
    stroke: currentColor;
    stroke-linecap: round;
    stroke-dasharray: 100;
    transition: stroke-dashoffset 160ms linear;
  }

  .header-update-progress-ring--indeterminate {
    animation: header-update-progress-spin 0.8s linear infinite;
  }

  .header-update-progress-ring--indeterminate .header-update-progress-value {
    stroke-dasharray: 28 72;
    stroke-dashoffset: 0 !important;
  }

  @keyframes header-update-action-spin {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes header-update-progress-spin {
    from {
      transform: rotate(-90deg);
    }
    to {
      transform: rotate(270deg);
    }
  }

  @media (max-width: 768px) {
    .header-update-status {
      gap: 1px;
    }
  }
</style>
