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
    spinning: boolean;
    progress: boolean;
  }

  const actionPresentation = $derived.by((): ActionPresentation => {
    if (update && !update.installability.installable) {
      return {
        tone: 'error',
        icon: 'warning',
        spinning: false,
        progress: false,
      };
    }

    switch (phase) {
      case 'checking':
        return {
          tone: 'checking',
          icon: 'refresh',
          spinning: true,
          progress: false,
        };
      case 'latest':
        return {
          tone: 'latest',
          icon: 'check-circle',
          spinning: false,
          progress: false,
        };
      case 'available':
        return {
          tone: 'available',
          icon: 'download',
          spinning: false,
          progress: false,
        };
      case 'downloading':
        return {
          tone: 'downloading',
          icon: 'download',
          spinning: false,
          progress: true,
        };
      case 'ready':
        return {
          tone: 'ready',
          icon: 'restart',
          spinning: false,
          progress: false,
        };
      case 'installing':
        return {
          tone: 'installing',
          icon: 'restart',
          spinning: true,
          progress: false,
        };
      case 'error':
        return {
          tone: 'error',
          icon: 'warning',
          spinning: false,
          progress: false,
        };
      default:
        return {
          tone: 'idle',
          icon: 'refresh',
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
  <div class="update-status" data-update-phase={phase} aria-live="polite">
    <span class="update-status-action-slot">
      <button
        type="button"
        class={`btn-icon btn-icon--md update-status-action update-status-action--${actionPresentation.tone}`}
        aria-label={actionTitle}
        title={actionTitle}
        aria-busy={phase === 'checking' || phase === 'downloading' || phase === 'installing'}
        aria-disabled={actionDisabled}
        onclick={activateAction}
      >
        {#if actionPresentation.progress}
          <span class="update-status-progress">
            <svg
              class="update-status-progress-ring"
              class:update-status-progress-ring--indeterminate={progress?.percent === undefined}
              viewBox="0 0 20 20"
              aria-hidden="true"
            >
              <circle class="update-status-progress-track" cx="10" cy="10" r="8" pathLength="100"></circle>
              <circle
                class="update-status-progress-value"
                cx="10"
                cy="10"
                r="8"
                pathLength="100"
                style:stroke-dashoffset={100 - downloadProgress}
              ></circle>
            </svg>
            {#if progress?.percent !== undefined}
              <span class="update-status-progress-percent">{progress.percent}</span>
            {/if}
          </span>
        {:else}
          <Icon
            name={actionPresentation.icon}
            size={14}
            class={`update-status-action-icon${actionPresentation.spinning ? ' update-status-action-icon--spinning' : ''}`}
          />
        {/if}
      </button>
    </span>
    <span class="update-status-version">v{currentVersion}</span>
  </div>
{/if}

<style>
  .update-status {
    display: inline-flex;
    align-items: center;
    gap: 1px;
    flex: 0 0 auto;
    min-width: 0;
  }

  .update-status-version {
    flex: 0 0 auto;
    height: var(--btn-height-md);
    padding: 0 4px;
    display: inline-flex;
    align-items: center;
    color: var(--foreground-muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .update-status-action-slot {
    display: inline-flex;
    flex: 0 0 auto;
  }

  /* 尺寸、圆角、hover 由全局 .btn-icon 提供，这里只表达更新阶段的色调。 */
  .update-status-action {
    color: var(--update-tone);
  }

  .update-status-action--idle {
    --update-tone: var(--foreground-muted);
  }

  .update-status-action--checking {
    --update-tone: var(--primary);
  }

  .update-status-action--latest {
    --update-tone: var(--success, #16a34a);
  }

  .update-status-action--available {
    --update-tone: var(--warning, #d97706);
  }

  .update-status-action--downloading {
    --update-tone: var(--color-codex, var(--info, #3b82f6));
  }

  .update-status-action--ready {
    --update-tone: var(--success, #16a34a);
  }

  .update-status-action--installing {
    --update-tone: var(--color-orchestrator, #8b5cf6);
  }

  .update-status-action--error {
    --update-tone: var(--error, #dc2626);
  }

  .update-status-action--idle:hover:not([aria-disabled='true']) {
    color: var(--foreground);
  }

  .update-status-action[aria-disabled='true'] {
    cursor: default;
    opacity: 0.86;
  }

  :global(.update-status-action-icon) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transform-origin: center;
  }

  :global(.update-status-action-icon--spinning) {
    will-change: transform;
    animation: update-status-action-spin 0.8s linear infinite;
  }

  .update-status-progress {
    position: relative;
    display: inline-flex;
    width: 20px;
    height: 20px;
    align-items: center;
    justify-content: center;
  }

  .update-status-progress-ring {
    width: 20px;
    height: 20px;
    flex: 0 0 20px;
    overflow: visible;
    transform: rotate(-90deg);
  }

  .update-status-progress-track,
  .update-status-progress-value {
    fill: none;
    stroke-width: 2;
  }

  .update-status-progress-track {
    stroke: color-mix(in srgb, currentColor 20%, transparent);
  }

  .update-status-progress-value {
    stroke: currentColor;
    stroke-linecap: round;
    stroke-dasharray: 100;
    transition: stroke-dashoffset 160ms linear;
  }

  .update-status-progress-ring--indeterminate {
    animation: update-status-progress-spin 0.8s linear infinite;
  }

  .update-status-progress-ring--indeterminate .update-status-progress-value {
    stroke-dasharray: 28 72;
    stroke-dashoffset: 0 !important;
  }

  .update-status-progress-percent {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: currentColor;
    font-size: 6.5px;
    font-weight: var(--font-semibold);
    font-variant-numeric: tabular-nums;
    line-height: 1;
    pointer-events: none;
  }

  @keyframes update-status-action-spin {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes update-status-progress-spin {
    from {
      transform: rotate(-90deg);
    }
    to {
      transform: rotate(270deg);
    }
  }

  @media (max-width: 768px) {
    .update-status {
      gap: 1px;
    }
  }
</style>
