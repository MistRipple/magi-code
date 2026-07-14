<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    checkDesktopUpdate,
    isDesktopRuntime,
    type DesktopUpdateInfo,
    type DesktopUpdateProgress,
  } from '../lib/desktop-updater';

  type PromptState = 'idle' | 'checking' | 'available' | 'installing' | 'error';

  let promptState = $state<PromptState>('idle');
  let update = $state<DesktopUpdateInfo | null>(null);
  let progress = $state<DesktopUpdateProgress | null>(null);
  let error = $state('');

  async function checkForUpdate(): Promise<void> {
    if (!isDesktopRuntime() || promptState === 'checking' || promptState === 'installing') {
      return;
    }
    promptState = 'checking';
    error = '';
    try {
      update = await checkDesktopUpdate();
      promptState = update ? 'available' : 'idle';
    } catch {
      promptState = 'idle';
      error = '';
    }
  }

  async function installUpdate(): Promise<void> {
    if (!update || promptState !== 'available') {
      return;
    }
    promptState = 'installing';
    progress = null;
    error = '';
    try {
      await update.install((nextProgress: DesktopUpdateProgress) => {
        progress = nextProgress;
      });
    } catch (reason) {
      promptState = 'error';
      error = reason instanceof Error ? reason.message : String(reason);
    }
  }

  async function dismiss(): Promise<void> {
    await update?.close().catch(() => undefined);
    update = null;
    progress = null;
    error = '';
    promptState = 'idle';
  }

  onMount(() => {
    const timer = setTimeout(() => void checkForUpdate(), 1200);
    return () => {
      clearTimeout(timer);
      void update?.close();
    };
  });
</script>

{#if update || promptState === 'error'}
  <aside class="desktop-update-prompt" aria-live="polite">
    <div class="prompt-icon" class:error={promptState === 'error'}>
      <Icon name={promptState === 'error' ? 'warning' : promptState === 'installing' ? 'download' : 'sparkles'} size={17} />
    </div>
    <div class="prompt-content">
      <strong>
        {#if promptState === 'installing'}
          {i18n.t('app.update.installing')}
        {:else if promptState === 'error'}
          {i18n.t('app.update.failed')}
        {:else}
          {i18n.t('app.update.title', { version: update?.version || '' })}
        {/if}
      </strong>
      {#if promptState === 'installing'}
        <span>
          {progress?.percent !== undefined
            ? i18n.t('app.update.progress', { percent: progress.percent })
            : i18n.t('app.update.downloading')}
        </span>
      {:else if promptState === 'error'}
        <span title={error}>{error || i18n.t('app.update.retryHint')}</span>
      {:else if update?.body}
        <span>{update.body}</span>
      {/if}
    </div>
    {#if promptState === 'available'}
      <button type="button" class="primary-action" onclick={() => void installUpdate()}>
        {i18n.t('app.update.install')}
      </button>
      <button type="button" class="secondary-action" onclick={() => void dismiss()}>
        {i18n.t('app.update.later')}
      </button>
    {:else if promptState === 'error'}
      <button type="button" class="primary-action" onclick={() => void installUpdate()}>
        {i18n.t('app.update.retry')}
      </button>
      <button type="button" class="icon-action" onclick={() => void dismiss()} title={i18n.t('app.update.close')} aria-label={i18n.t('app.update.close')}>
        <Icon name="close" size={14} />
      </button>
    {/if}
  </aside>
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
  .secondary-action {
    padding: 5px 8px;
  }

  .primary-action {
    color: var(--background);
    background: var(--accent);
  }

  .secondary-action,
  .icon-action {
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

  @media (max-width: 680px) {
    .desktop-update-prompt {
      top: 46px;
      right: 8px;
      width: calc(100vw - 16px);
    }

    .secondary-action {
      display: none;
    }
  }
</style>
