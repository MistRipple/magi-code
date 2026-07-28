<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    getDesktopRuntimeRecovery,
    restartDesktopRuntime,
    type DesktopRuntimeRecoverySnapshot,
  } from '../lib/desktop-runtime-recovery';

  let snapshot = $state<DesktopRuntimeRecoverySnapshot | null>(null);
  let loading = $state(true);
  let restarting = $state(false);
  let confirmingExternal = $state(false);
  let confirmingEnvironmentRecovery = $state(false);
  let actionError = $state('');

  const isBusy = $derived(loading || restarting || snapshot?.status === 'restarting');
  const title = $derived.by(() => {
    switch (snapshot?.status) {
      case 'port-occupied': return i18n.t('app.runtimePortOccupied', { port: snapshot.port });
      case 'failed': return i18n.t('app.runtimeStartFailed');
      case 'ready': return i18n.t('app.runtimeConnectionInterrupted');
      case 'restarting': return i18n.t('app.runtimeRestarting');
      default: return i18n.t('app.runtimeUnavailable');
    }
  });
  const hint = $derived.by(() => {
    if (confirmingExternal) {
      return i18n.t('app.runtimeExternalConfirm');
    }
    if (confirmingEnvironmentRecovery) {
      return i18n.t('app.runtimeEnvironmentRecoveryConfirm');
    }
    if (snapshot?.status === 'port-occupied') {
      return snapshot.requiresConfirmation
        ? i18n.t('app.runtimePortOccupiedExternalHint')
        : i18n.t('app.runtimePortOccupiedMagiHint');
    }
    if (snapshot?.status === 'ready') {
      return i18n.t('app.runtimeReadyButDisconnectedHint');
    }
    if (snapshot?.status === 'failed' && !snapshot.canRestart) {
      return i18n.t('app.runtimeManualRecoveryHint');
    }
    return i18n.t('app.runtimeUnavailableHint');
  });

  async function refreshDiagnosis(): Promise<void> {
    loading = true;
    actionError = '';
    confirmingExternal = false;
    confirmingEnvironmentRecovery = false;
    try {
      snapshot = await getDesktopRuntimeRecovery();
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      loading = false;
    }
  }

  async function runRecovery(confirmExternalProcesses: boolean): Promise<void> {
    if (!snapshot) return;
    restarting = true;
    actionError = '';
    try {
      const recovered = await restartDesktopRuntime(snapshot, confirmExternalProcesses);
      snapshot = recovered;
      if (recovered.status === 'ready') {
        window.location.reload();
      }
    } catch (error) {
      const restartError = error instanceof Error ? error.message : String(error);
      await refreshDiagnosis();
      actionError = restartError;
    } finally {
      restarting = false;
    }
  }

  function requestRecovery(): void {
    if (!snapshot) return;
    if (snapshot.requiresConfirmation && !confirmingExternal) {
      confirmingExternal = true;
      return;
    }
    void runRecovery(snapshot.requiresConfirmation);
  }

  function requestEnvironmentRecovery(): void {
    if (!snapshot) return;
    if (snapshot.requiresConfirmation) {
      confirmingExternal = true;
      return;
    }
    confirmingEnvironmentRecovery = true;
  }

  onMount(() => {
    void refreshDiagnosis();
  });
</script>

<div class="runtime-recovery" role="status" aria-live="polite">
  <div class="runtime-recovery__icon" class:runtime-recovery__icon--spinning={isBusy}>
    <Icon name={isBusy ? 'loader' : 'warning'} size={30} />
  </div>
  <h2>{title}</h2>
  <p class="runtime-recovery__hint">{hint}</p>

  {#if snapshot?.occupants.length}
    <div class="runtime-recovery__processes" aria-label={i18n.t('app.runtimeOccupyingProcesses')}>
      {#each snapshot.occupants as occupant (occupant.pid)}
        <div class="runtime-recovery__process">
          <span>{occupant.processName}</span>
          <code>PID {occupant.pid}</code>
          {#if occupant.executablePath}
            <span class="runtime-recovery__path" title={occupant.executablePath}>{occupant.executablePath}</span>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  {#if actionError}
    <p class="runtime-recovery__error">{actionError}</p>
  {/if}

  <div class="runtime-recovery__actions">
    {#if confirmingExternal || confirmingEnvironmentRecovery}
      <button type="button" class="runtime-recovery__button secondary" onclick={() => { confirmingExternal = false; confirmingEnvironmentRecovery = false; }} disabled={isBusy}>
        {i18n.t('common.cancel')}
      </button>
      <button type="button" class="runtime-recovery__button danger" onclick={() => void runRecovery(confirmingExternal)} disabled={isBusy}>
        <Icon name="restart" size={14} />
        <span>{confirmingExternal ? i18n.t('app.runtimeConfirmStopAndRestart') : i18n.t('app.runtimeConfirmEnvironmentRecovery')}</span>
      </button>
    {:else if snapshot?.status === 'ready'}
      {#if snapshot.canRestart}
        <button type="button" class="runtime-recovery__button primary" onclick={requestEnvironmentRecovery} disabled={isBusy}>
          <Icon name="restart" size={14} />
          <span>{i18n.t('app.runtimeRestoreEnvironment')}</span>
        </button>
      {/if}
    {:else if snapshot?.canRestart}
      <button type="button" class="runtime-recovery__button primary" onclick={requestRecovery} disabled={isBusy}>
        <Icon name="restart" size={14} />
        <span>{snapshot.requiresConfirmation ? i18n.t('app.runtimeStopAndRestart') : i18n.t('app.runtimeRestart')}</span>
      </button>
    {/if}
    <button type="button" class="runtime-recovery__button secondary" onclick={() => void refreshDiagnosis()} disabled={isBusy}>
      <Icon name="refresh" size={14} />
      <span>{i18n.t('app.runtimeRediagnose')}</span>
    </button>
  </div>

  {#if snapshot?.technicalDetail}
    <details class="runtime-recovery__details">
      <summary>{i18n.t('app.runtimeTechnicalDetails')}</summary>
      <pre>{snapshot.technicalDetail}</pre>
    </details>
  {/if}
</div>

<style>
  .runtime-recovery {
    display: flex;
    flex-direction: column;
    align-items: center;
    width: min(560px, calc(100% - 40px));
    color: var(--foreground);
    text-align: center;
  }

  .runtime-recovery__icon {
    display: grid;
    place-items: center;
    width: 42px;
    height: 42px;
    margin-bottom: 14px;
    color: var(--warning, #d99b28);
  }

  .runtime-recovery__icon--spinning {
    color: var(--foreground-muted);
    animation: runtime-recovery-spin 1.1s linear infinite;
  }

  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
    letter-spacing: 0;
  }

  .runtime-recovery__hint {
    max-width: 480px;
    margin: 8px 0 0;
    color: var(--foreground-muted);
    font-size: 13px;
    line-height: 1.6;
  }

  .runtime-recovery__processes {
    width: min(520px, 100%);
    margin-top: 18px;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }

  .runtime-recovery__process {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 4px 12px;
    padding: 10px 2px;
    text-align: left;
    font-size: 12px;
  }

  .runtime-recovery__process + .runtime-recovery__process {
    border-top: 1px solid var(--border);
  }

  .runtime-recovery__process code {
    color: var(--foreground-muted);
    font-family: var(--font-mono);
  }

  .runtime-recovery__path {
    grid-column: 1 / -1;
    overflow: hidden;
    color: var(--foreground-subtle, var(--foreground-muted));
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .runtime-recovery__error {
    max-width: 520px;
    margin: 14px 0 0;
    color: var(--destructive, #d65c5c);
    font-size: 12px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .runtime-recovery__actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    margin-top: 20px;
  }

  .runtime-recovery__button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    min-height: 34px;
    padding: 0 13px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    font-size: 13px;
    cursor: pointer;
  }

  .runtime-recovery__button:disabled {
    cursor: default;
    opacity: 0.55;
  }

  .runtime-recovery__button.primary {
    border-color: var(--primary);
    background: var(--primary);
    color: var(--primary-foreground);
  }

  .runtime-recovery__button.secondary {
    background: var(--background);
    color: var(--foreground);
  }

  .runtime-recovery__button.danger {
    border-color: var(--destructive, #c84e4e);
    background: var(--destructive, #c84e4e);
    color: #fff;
  }

  .runtime-recovery__details {
    width: min(520px, 100%);
    margin-top: 18px;
    color: var(--foreground-muted);
    text-align: left;
    font-size: 12px;
  }

  .runtime-recovery__details summary {
    cursor: pointer;
  }

  .runtime-recovery__details pre {
    max-height: 150px;
    margin: 8px 0 0;
    padding: 10px 0;
    overflow: auto;
    border-top: 1px solid var(--border);
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  @keyframes runtime-recovery-spin {
    to { transform: rotate(360deg); }
  }
</style>
