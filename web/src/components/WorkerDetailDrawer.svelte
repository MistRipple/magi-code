<script lang="ts">
  import { fade, fly } from 'svelte/transition';
  import { onMount, tick } from 'svelte';
  import type { AgentId, TimelineRenderItem } from '../types/message';
  import { buildTimelineRenderItems } from '../lib/timeline-render-items';
  import { getAgentVisualInfo } from '../lib/agent-colors';
  import { resolveWorkerDisplayName, resolveWorkerRoleSource } from '../lib/worker-role-utils';
  import { getEnabledAgents, messagesState } from '../stores/messages.svelte';
  import {
    workerDetailDrawerState,
    closeWorkerDetailDrawer,
  } from '../stores/worker-detail-drawer.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageList from './MessageList.svelte';

  let dialogEl: HTMLDivElement | undefined = $state();
  let previouslyFocused: HTMLElement | null = null;

  const workerTabId = $derived(workerDetailDrawerState.activeWorkerTabId);
  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(messagesState.settingsRegistrySnapshot);

  const workerMeta = $derived.by(() => {
    const id = workerTabId;
    if (!id) {
      return null;
    }
    const roleSource = resolveWorkerRoleSource(id, enabledAgents, registrySnapshot);
    const displayWorkerId = (roleSource?.templateId && roleSource.templateId.trim()) || id;
    const displayName = resolveWorkerDisplayName(displayWorkerId, enabledAgents, registrySnapshot, (key) => i18n.t(key))
      || displayWorkerId;
    const visualInfo = getAgentVisualInfo(displayWorkerId, roleSource?.colorToken);
    return {
      id,
      displayName,
      color: visualInfo.color,
      muted: visualInfo.muted,
      icon: visualInfo.icon,
    };
  });

  const renderItems = $derived.by<TimelineRenderItem[]>(() => {
    const id = workerTabId;
    const projection = messagesState.canonicalTimelineProjection;
    if (!id || !projection) {
      return [];
    }
    return buildTimelineRenderItems(projection, 'worker', id as AgentId);
  });

  const stageCounts = $derived.by(() => {
    const kinds = new Map<string, number>();
    for (const item of renderItems) {
      const kind = typeof item.message.metadata?.turnItemKind === 'string'
        ? item.message.metadata.turnItemKind
        : '';
      if (!kind) continue;
      kinds.set(kind, (kinds.get(kind) || 0) + 1);
    }
    return {
      toolCalls: kinds.get('tool_call') || 0,
      replies: kinds.get('assistant_text') || 0,
      thinking: kinds.get('assistant_thinking') || 0,
    };
  });

  function handleBackdropKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.stopPropagation();
      closeWorkerDetailDrawer();
    }
    if (event.key === 'Tab' && dialogEl) {
      const focusable = dialogEl.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (focusable.length === 0) {
        event.preventDefault();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
  }

  function handleBackdropClick(event: MouseEvent) {
    if (event.target === event.currentTarget) {
      closeWorkerDetailDrawer();
    }
  }

  onMount(() => {
    previouslyFocused = document.activeElement as HTMLElement;
    tick().then(() => {
      if (!dialogEl) return;
      const focusable = dialogEl.querySelector<HTMLElement>(
        'button:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (focusable) {
        focusable.focus();
      } else {
        dialogEl.focus();
      }
    });
    return () => {
      if (previouslyFocused && typeof previouslyFocused.focus === 'function') {
        previouslyFocused.focus();
      }
    };
  });
</script>

{#if workerTabId && workerMeta}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="worker-detail-overlay"
    role="presentation"
    onclick={handleBackdropClick}
    onkeydown={handleBackdropKeydown}
    transition:fade={{ duration: 160 }}
  >
    <div
      bind:this={dialogEl}
      class="worker-detail-drawer"
      role="dialog"
      aria-modal="true"
      aria-labelledby="worker-detail-title"
      tabindex="-1"
      transition:fly={{ x: 24, duration: 220, opacity: 0 }}
    >
      <header class="worker-detail-header" style={`--worker-color:${workerMeta.color};--worker-muted:${workerMeta.muted};`}>
        <div class="worker-detail-header__identity">
          <span class="worker-detail-header__icon" aria-hidden="true">
            <Icon name={workerMeta.icon} size={16} />
          </span>
          <div class="worker-detail-header__text">
            <span class="worker-detail-header__eyebrow">{i18n.t('workerDetailDrawer.eyebrow')}</span>
            <h2 id="worker-detail-title" class="worker-detail-header__title">{workerMeta.displayName}</h2>
          </div>
        </div>
        <div class="worker-detail-header__meta">
          {#if stageCounts.toolCalls > 0}
            <span class="worker-detail-header__stat">{i18n.t('workerDetailDrawer.stats.toolCalls', { count: stageCounts.toolCalls })}</span>
          {/if}
          {#if stageCounts.replies > 0}
            <span class="worker-detail-header__stat">{i18n.t('workerDetailDrawer.stats.replies', { count: stageCounts.replies })}</span>
          {/if}
          {#if stageCounts.thinking > 0}
            <span class="worker-detail-header__stat">{i18n.t('workerDetailDrawer.stats.thinking', { count: stageCounts.thinking })}</span>
          {/if}
        </div>
        <button
          type="button"
          class="worker-detail-header__close"
          aria-label={i18n.t('workerDetailDrawer.close')}
          onclick={closeWorkerDetailDrawer}
        >
          <Icon name="x" size={16} />
        </button>
      </header>

      <div class="worker-detail-body">
        <MessageList
          workerName={workerTabId as AgentId}
          renderItems={renderItems}
          displayContext="worker"
          readOnly={true}
          emptyState={{
            icon: 'clock',
            title: i18n.t('workerDetailDrawer.empty.title'),
            hint: i18n.t('workerDetailDrawer.empty.hint'),
          }}
        />
      </div>
    </div>
  </div>
{/if}

<style>
  .worker-detail-overlay {
    position: fixed;
    inset: 0;
    z-index: var(--z-drawer, 60);
    background: var(--overlay, rgba(0, 0, 0, 0.35));
    display: flex;
    justify-content: flex-end;
    align-items: stretch;
  }

  .worker-detail-drawer {
    width: min(640px, 100vw);
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--assistant-message-bg);
    border-left: 1px solid var(--border);
    box-shadow: -16px 0 48px rgba(0, 0, 0, 0.18);
    min-height: 0;
  }

  .worker-detail-header {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--worker-muted, var(--surface)) 50%, var(--assistant-message-bg));
  }

  .worker-detail-header__identity {
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }

  .worker-detail-header__icon {
    width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 999px;
    color: var(--worker-color, var(--primary));
    background: var(--worker-muted, color-mix(in srgb, var(--primary) 14%, transparent));
    flex-shrink: 0;
  }

  .worker-detail-header__text {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .worker-detail-header__eyebrow {
    color: var(--foreground-muted);
    font-size: 11px;
    letter-spacing: 0.02em;
  }

  .worker-detail-header__title {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-base);
    font-weight: var(--font-semibold);
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .worker-detail-header__meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .worker-detail-header__stat {
    display: inline-flex;
    align-items: center;
    padding: 2px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
    white-space: nowrap;
  }

  .worker-detail-header__close {
    width: 32px;
    height: 32px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    border-radius: var(--radius-md);
    cursor: pointer;
    transition: background var(--transition-base, 180ms) ease-out, color var(--transition-base, 180ms) ease-out;
  }

  .worker-detail-header__close:hover {
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
    color: var(--foreground);
  }

  .worker-detail-header__close:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  .worker-detail-body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  @media (max-width: 640px) {
    .worker-detail-drawer {
      width: 100vw;
    }
    .worker-detail-header {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .worker-detail-header__meta {
      grid-column: 1 / -1;
    }
  }
</style>
