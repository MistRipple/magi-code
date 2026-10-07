<script lang="ts">
  import type { ComposerWorkspaceOption } from '../stores/composer-workspace.svelte';
  import { addToast } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    discardSessionIsolation,
    enableSessionIsolation,
    getSessionIsolation,
    openIsolationMergeDialog,
    type IsolationBinding,
  } from '../stores/session-isolation-store.svelte';
  import { isolationOriginBlockingSession } from '../lib/isolation-merge';
  import Icon from './Icon.svelte';

  interface Props {
    workspace: ComposerWorkspaceOption | null;
    sessionId?: string;
    disabled?: boolean;
    /** 草稿会话还没有会话 ID：需要隔离时先把草稿落成真实会话，返回它的 ID。 */
    ensureSession?: () => Promise<string>;
  }

  let { workspace, sessionId = '', disabled = false, ensureSession }: Props = $props();
  let open = $state(false);
  let busy = $state(false);

  const binding = $derived.by<IsolationBinding | null>(() => {
    if (!workspace) return null;
    return {
      workspaceId: workspace.workspaceId,
      workspacePath: workspace.rootPathRef?.trim() || workspace.rootPath.trim(),
    };
  });
  const isolation = $derived(sessionId.trim() ? getSessionIsolation(sessionId) : null);
  const visible = $derived(Boolean(binding));
  const automatic = $derived(Boolean(isolation && isolationOriginBlockingSession(isolation.origin)));
  const title = $derived.by(() => {
    if (!isolation) return i18n.t('isolation.chip.offTitle');
    return automatic ? i18n.t('isolation.chip.autoTitle') : i18n.t('isolation.chip.onTitle');
  });

  async function enable(): Promise<void> {
    if (!binding || busy) return;
    busy = true;
    try {
      let targetSessionId = sessionId.trim();
      if (!targetSessionId) {
        // 草稿会话：先落成真实会话，再让它在隔离副本里开始。
        if (!ensureSession) return;
        targetSessionId = (await ensureSession()).trim();
        if (!targetSessionId) return;
      }
      const result = await enableSessionIsolation(targetSessionId, binding);
      if (result.ok) {
        addToast('success', i18n.t('isolation.enable.done'));
      } else {
        addToast('error', result.error || i18n.t('isolation.enable.failed'));
      }
    } catch (error) {
      addToast('error', error instanceof Error && error.message ? error.message : i18n.t('isolation.enable.failed'));
    } finally {
      busy = false;
    }
  }

  function toggle(): void {
    if (disabled || busy || !binding) return;
    if (!isolation) {
      void enable();
      return;
    }
    open = !open;
  }

  function openMerge(): void {
    if (!binding) return;
    open = false;
    openIsolationMergeDialog(sessionId, binding);
  }

  async function discard(): Promise<void> {
    if (!binding || busy) return;
    open = false;
    if (!window.confirm(i18n.t('isolation.discard.confirm'))) return;
    busy = true;
    try {
      const result = await discardSessionIsolation(sessionId, binding, true);
      addToast(
        result.ok ? 'success' : 'error',
        result.ok ? i18n.t('isolation.discard.done') : result.error || i18n.t('isolation.discard.failed'),
      );
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    const handleOutside = (event: PointerEvent) => {
      if (open && !(event.target instanceof Element && event.target.closest('.session-isolation-control'))) {
        open = false;
      }
    };
    document.addEventListener('pointerdown', handleOutside, true);
    return () => document.removeEventListener('pointerdown', handleOutside, true);
  });
</script>

{#if visible}
  <div class="session-isolation-control">
    <button
      type="button"
      class="session-isolation-trigger"
      class:on={Boolean(isolation)}
      class:active={open}
      {title}
      aria-pressed={Boolean(isolation)}
      aria-expanded={isolation ? open : undefined}
      disabled={disabled || busy}
      onclick={toggle}
    >
      <Icon name="layers" size={12} />
      <span class="session-isolation-label">
        {isolation ? i18n.t('isolation.chip.on') : i18n.t('isolation.chip.off')}
      </span>
    </button>

    {#if open && isolation}
      <div class="session-isolation-popover" data-magi-surface="popover" role="menu">
        <div class="session-isolation-heading">{i18n.t('isolation.chip.menuTitle')}</div>
        <button type="button" class="session-isolation-item" role="menuitem" onclick={openMerge}>
          <Icon name="git-merge" size={12} />
          <span>{i18n.t('isolation.menu.merge')}</span>
        </button>
        <button
          type="button"
          class="session-isolation-item session-isolation-item--danger"
          role="menuitem"
          onclick={() => void discard()}
        >
          <Icon name="trash" size={12} />
          <span>{i18n.t('isolation.menu.discard')}</span>
        </button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .session-isolation-control {
    position: relative;
    flex: 0 1 auto;
    min-width: 0;
  }

  .session-isolation-trigger {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--surface-1) 88%, transparent);
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: 11px;
    white-space: nowrap;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .session-isolation-trigger:hover:not(:disabled),
  .session-isolation-trigger.active {
    border-color: color-mix(in srgb, var(--primary) 38%, var(--border-subtle));
    background: color-mix(in srgb, var(--primary) 12%, var(--surface-1));
    color: var(--primary);
  }

  .session-isolation-trigger.on {
    border-color: color-mix(in srgb, var(--primary) 36%, transparent);
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    color: var(--primary);
  }

  .session-isolation-trigger:disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }

  .session-isolation-popover {
    position: absolute;
    bottom: calc(100% + 7px);
    left: 0;
    z-index: 34;
    width: 200px;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--dropdown-bg);
    box-shadow: 0 14px 36px rgba(0, 0, 0, 0.32);
  }

  .session-isolation-heading {
    padding: 4px 8px 6px;
    color: var(--foreground-muted);
    font-size: 11px;
  }

  .session-isolation-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    min-height: 30px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    cursor: pointer;
    font-size: 12px;
    text-align: left;
  }

  .session-isolation-item:hover {
    background: var(--surface-hover);
  }

  .session-isolation-item--danger:hover {
    color: var(--error);
  }

  @container magi-composer (max-width: 800px) {
    .session-isolation-trigger {
      width: 28px;
      padding: 0;
      justify-content: center;
    }

    .session-isolation-label {
      display: none;
    }
  }
</style>
