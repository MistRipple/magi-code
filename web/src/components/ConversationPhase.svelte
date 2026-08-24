<script lang="ts">
  import { untrack } from 'svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import {
    resolveConversationPhaseSummary,
    type ConversationPhase as ConversationPhaseModel,
  } from '../lib/conversation-disclosure';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import ConversationProcessRow from './ConversationProcessRow.svelte';
  import ConversationToolGroup from './ConversationToolGroup.svelte';

  interface Props {
    phase: ConversationPhaseModel;
    active?: boolean;
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
    continueInterruptedSession: () => void;
  }

  let {
    phase,
    active = false,
    readOnly = false,
    displayContext = 'thread',
    filePreviewScopeForItem,
    continueInterruptedSession,
  }: Props = $props();

  // 自动状态只负责“当前阶段展开、离开当前阶段后收起”；用户手动操作优先。
  let expanded = $state(untrack(() => active));
  let manualOverride = false;
  let previousActive = $state(untrack(() => active));

  $effect(() => {
    const nextActive = active;
    if (nextActive !== previousActive) {
      if (!manualOverride) expanded = nextActive;
      previousActive = nextActive;
    }
  });

  const contentId = $derived(`conversation-phase-${phase.key.replace(/[^a-zA-Z0-9_-]/gu, '-')}`);
  const summary = $derived(
    resolveConversationPhaseSummary(phase, i18n.t.bind(i18n)),
  );
  // 进行中也保留当前阶段的具体摘要，状态点单独表达“正在执行”。
  const headerLabel = $derived(summary);

  function toggle(): void {
    manualOverride = true;
    expanded = !expanded;
  }
</script>

<section
  class="conversation-phase"
  class:expanded
  class:active
  data-conversation-phase={phase.key}
  data-conversation-phase-state={active ? 'active' : 'completed'}
>
  <button
    type="button"
    class="conversation-phase-header"
    aria-expanded={expanded}
    aria-controls={contentId}
    onclick={toggle}
  >
    <span class="conversation-phase-chevron" class:rotated={expanded}>
      <Icon name="chevron-right" size={12} />
    </span>
    <span class="conversation-phase-label" title={summary}>{headerLabel}</span>
    {#if active}
      <span class="conversation-phase-live" aria-label={i18n.t('messageList.turnDisclosure.processing')}>
        <span class="conversation-phase-live-dot"></span>
      </span>
    {/if}
  </button>

  {#if expanded}
    <div class="conversation-phase-content" id={contentId}>
      {#each phase.entries as entry (entry.key)}
        {#if entry.kind === 'event'}
          <div class="turn-process-entry">
            <ConversationProcessRow item={entry.item} />
          </div>
        {:else}
          <div class="turn-process-entry">
            <ConversationToolGroup
              items={entry.items}
              {readOnly}
              {displayContext}
              {filePreviewScopeForItem}
              {continueInterruptedSession}
            />
          </div>
        {/if}
      {/each}
    </div>
  {/if}
</section>

<style>
  .conversation-phase {
    min-width: 0;
    margin: 0 0 1px;
  }

  .conversation-phase-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 28px;
    gap: 7px;
    padding: 0;
    border: 0;
    border-radius: 0;
    background: transparent;
    color: var(--foreground-muted);
    text-align: left;
    cursor: pointer;
    transition: color var(--transition-fast);
  }

  .conversation-phase-header:hover,
  .conversation-phase-header:focus-visible {
    background: transparent;
    color: var(--foreground);
  }

  .conversation-phase-header:focus-visible {
    outline: 1px solid color-mix(in srgb, var(--primary) 72%, transparent);
    outline-offset: 2px;
    border-radius: var(--radius-sm);
  }

  .conversation-phase-chevron {
    display: inline-flex;
    flex: 0 0 12px;
    color: currentColor;
    opacity: 0.68;
    transition: transform var(--transition-fast), opacity var(--transition-fast);
  }

  .conversation-phase-chevron.rotated,
  .conversation-phase-header:hover .conversation-phase-chevron,
  .conversation-phase-header:focus-visible .conversation-phase-chevron {
    opacity: 1;
  }

  .conversation-phase-chevron.rotated {
    transform: rotate(90deg);
  }

  .conversation-phase-label {
    min-width: 0;
    overflow: hidden;
    color: currentColor;
    font-size: var(--text-sm);
    line-height: 1.3;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .conversation-phase-live {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    width: 12px;
    height: 12px;
    margin-left: auto;
  }

  .conversation-phase-live-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--success);
    animation: conversation-phase-pulse 1.5s ease-in-out infinite;
  }

  .conversation-phase-content {
    display: flex;
    flex-direction: column;
    gap: 0;
    min-width: 0;
    padding: 0 0 1px 8px;
    border-left: 1px solid color-mix(in srgb, var(--border) 74%, transparent);
  }

  .turn-process-entry {
    min-width: 0;
  }

  @keyframes conversation-phase-pulse {
    50% {
      opacity: 0.35;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .conversation-phase-live-dot {
      animation: none;
    }
  }

  @media (max-width: 560px) {
    .conversation-phase-header {
      min-height: 32px;
    }

    .conversation-phase-content {
      padding-left: 6px;
    }
  }
</style>
