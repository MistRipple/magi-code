<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    hasQueuedMessagesAcrossSessions,
    messagesState,
  } from '../stores/messages.svelte';
  import { composerWorkspaceState } from '../stores/composer-workspace.svelte';
  import {
    desktopUpdaterState,
    dismissDesktopUpdatePrompt,
    downloadDesktopUpdate,
    restartWithDesktopUpdate,
    retryDesktopUpdate,
    startDesktopUpdater,
  } from '../stores/desktop-updater.svelte';

  let restartConfirmationOpen = $state(false);

  const update = $derived(desktopUpdaterState.update);
  const phase = $derived(desktopUpdaterState.phase);
  const progress = $derived(desktopUpdaterState.progress);
  const error = $derived(desktopUpdaterState.error);
  const hasProtectedWork = $derived(
    messagesState.isProcessing
    || messagesState.pendingRequests.size > 0
    || hasQueuedMessagesAcrossSessions()
    || messagesState.editingTurn !== null
    || composerWorkspaceState.hasUnsavedInput
    || messagesState.sessions.some((session) => (
      session.isRunning === true || (session.runningTaskCount ?? 0) > 0
    )),
  );
  const promptVisible = $derived(
    !desktopUpdaterState.promptDismissed
    && (
      (update !== null && ['available', 'downloading', 'ready', 'installing'].includes(phase))
      || (phase === 'error' && desktopUpdaterState.errorStage !== 'check')
    ),
  );

  function requestRestart(): void {
    if (hasProtectedWork) {
      restartConfirmationOpen = true;
      return;
    }
    void restartWithDesktopUpdate();
  }

  function confirmRestart(): void {
    restartConfirmationOpen = false;
    void restartWithDesktopUpdate();
  }

  function deferUpdate(): void {
    restartConfirmationOpen = false;
    dismissDesktopUpdatePrompt();
  }

  onMount(startDesktopUpdater);
</script>

{#if promptVisible}
  <aside class="desktop-update-prompt" aria-live="polite" data-update-phase={phase}>
    <div class="prompt-icon" class:error={phase === 'error'} class:ready={phase === 'ready'}>
      <Icon
        name={phase === 'error'
          ? 'warning'
          : phase === 'downloading'
            ? 'download'
            : phase === 'ready' || phase === 'installing'
              ? 'refresh'
              : 'sparkles'}
        size={17}
      />
    </div>
    <div class="prompt-content">
      <strong>
        {#if phase === 'downloading'}
          {i18n.t('app.update.downloadingTitle')}
        {:else if phase === 'ready'}
          {i18n.t('app.update.readyTitle', { version: update?.version || '' })}
        {:else if phase === 'installing'}
          {i18n.t('app.update.restarting')}
        {:else if phase === 'error'}
          {i18n.t('app.update.failed')}
        {:else}
          {i18n.t('app.update.title', { version: update?.version || '' })}
        {/if}
      </strong>
      {#if phase === 'downloading'}
        <span>
          {progress?.percent !== undefined
            ? i18n.t('app.update.progress', { percent: progress.percent })
            : i18n.t('app.update.downloading')}
        </span>
        <div
          class="desktop-update-progress"
          role="progressbar"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={progress?.percent}
          aria-label={i18n.t('app.update.downloadingTitle')}
        >
          <span
            class="desktop-update-progress__fill"
            class:desktop-update-progress__fill--indeterminate={progress?.percent === undefined}
            style:width={progress?.percent !== undefined ? `${progress.percent}%` : undefined}
          ></span>
        </div>
      {:else if phase === 'ready'}
        <span>{i18n.t('app.update.readyHint')}</span>
      {:else if phase === 'installing'}
        <span>{i18n.t('app.update.restartingHint')}</span>
      {:else if phase === 'error'}
        <span title={error}>{error || i18n.t('app.update.retryHint')}</span>
      {:else if update?.body}
        <span>{update.body}</span>
      {:else}
        <span>{i18n.t('app.update.availableHint')}</span>
      {/if}
    </div>

    {#if phase === 'available'}
      <div class="prompt-actions">
        <button type="button" class="primary-action" onclick={() => void downloadDesktopUpdate()}>
          {i18n.t('app.update.download')}
        </button>
        <button type="button" class="secondary-action" onclick={deferUpdate}>
          {i18n.t('app.update.later')}
        </button>
      </div>
    {:else if phase === 'ready'}
      <div class="prompt-actions">
        <button type="button" class="primary-action" onclick={requestRestart}>
          {i18n.t('app.update.restartNow')}
        </button>
        <button type="button" class="secondary-action" onclick={deferUpdate}>
          {i18n.t('app.update.restartLater')}
        </button>
      </div>
    {:else if phase === 'error'}
      <div class="prompt-actions">
        <button type="button" class="primary-action" onclick={() => void retryDesktopUpdate()}>
          {i18n.t('app.update.retry')}
        </button>
        <button
          type="button"
          class="icon-action"
          onclick={deferUpdate}
          title={i18n.t('app.update.close')}
          aria-label={i18n.t('app.update.close')}
        >
          <Icon name="close" size={14} />
        </button>
      </div>
    {/if}
  </aside>
{/if}

{#if restartConfirmationOpen}
  <Modal
    size="sm"
    title={i18n.t('app.update.restartConfirmTitle')}
    closeOnEscape={true}
    closeOnBackdrop={false}
    onClose={() => (restartConfirmationOpen = false)}
  >
    <p class="restart-confirmation-text">{i18n.t('app.update.restartConfirmActiveWork')}</p>
    {#snippet footer()}
      <button type="button" class="modal-secondary" onclick={() => (restartConfirmationOpen = false)}>
        {i18n.t('app.update.restartLater')}
      </button>
      <button type="button" class="modal-primary" onclick={confirmRestart}>
        {i18n.t('app.update.restartAnyway')}
      </button>
    {/snippet}
  </Modal>
{/if}

<style>
  .desktop-update-prompt {
    position: fixed;
    top: 52px;
    right: 16px;
    z-index: 420;
    display: flex;
    align-items: center;
    gap: 10px;
    width: min(520px, calc(100vw - 32px));
    min-height: 52px;
    padding: 10px 12px;
    border: 1px solid var(--border-strong, var(--border));
    border-radius: 8px;
    background: var(--surface-elevated, var(--background));
    box-shadow: var(--shadow-lg);
  }

  .prompt-icon {
    display: grid;
    place-items: center;
    flex: 0 0 28px;
    width: 28px;
    height: 28px;
    color: var(--accent);
    border-radius: 6px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }

  .prompt-icon.ready {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 12%, transparent);
  }

  .prompt-icon.error {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 12%, transparent);
  }

  .prompt-content {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    gap: 2px;
  }

  .prompt-content strong,
  .prompt-content span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prompt-content strong {
    color: var(--foreground);
    font-size: 12px;
    font-weight: 600;
  }

  .prompt-content span {
    color: var(--foreground-muted);
    font-size: 11px;
  }

  .prompt-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 0 0 auto;
  }

  button {
    flex: 0 0 auto;
    border: 0;
    border-radius: 5px;
    cursor: pointer;
    font: inherit;
    font-size: 11px;
    white-space: nowrap;
  }

  .primary-action,
  .secondary-action,
  .modal-primary,
  .modal-secondary {
    padding: 5px 8px;
  }

  .primary-action,
  .modal-primary {
    color: var(--primary-foreground, var(--background));
    background: var(--accent);
  }

  .secondary-action,
  .icon-action,
  .modal-secondary {
    color: var(--foreground-muted);
    background: var(--surface-hover);
  }

  .icon-action {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    padding: 0;
  }

  .restart-confirmation-text {
    margin: 0;
    color: var(--foreground-muted);
    font-size: 13px;
    line-height: 1.55;
  }

  @media (max-width: 680px) {
    .desktop-update-prompt {
      top: 46px;
      right: 8px;
      align-items: flex-start;
      flex-wrap: wrap;
      width: calc(100vw - 16px);
    }

    .prompt-actions {
      width: 100%;
      justify-content: flex-end;
      padding-left: 38px;
    }
  }
</style>
