<script lang="ts">
  import { untrack } from 'svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import {
    resolveConversationPhaseDetails,
    resolveConversationPhasePresentation,
    resolveConversationPhaseSummary,
    type ConversationPhase as ConversationPhaseModel,
  } from '../lib/conversation-disclosure';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';
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
  let manualOverride = $state(false);
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
  // 标题已经完整表达的阶段（例如一句短文字）展开后没有更多信息：
  // 只显示静态标题，不给一个点开后什么都不多的箭头。
  const details = $derived(resolveConversationPhaseDetails(phase));
  const expandable = $derived(details.length > 0);
  const presentation = $derived(
    resolveConversationPhasePresentation(phase, { active, expanded, manualOverride }),
  );
  // 收起时标题是内容摘要；展开后首条正文已经完整显示在下方，标题再写一遍同样的话就是重复，
  // 改为只表达阶段状态（状态点另外表达“正在执行”）。
  const headerLabel = $derived(
    presentation.headerRepeatsBody
      ? i18n.t(active ? 'messageList.turnDisclosure.processing' : 'messageList.turnDisclosure.processed')
      : summary,
  );

  function toggle(): void {
    manualOverride = true;
    expanded = !expanded;
  }
</script>

<section
  class="conversation-phase"
  class:expanded={expanded && expandable}
  class:active
  data-conversation-phase={phase.key}
  data-conversation-phase-state={active ? 'active' : 'completed'}
>
  {#if presentation.bodyOnly}
    <!-- 正在流式输出的一段文字：直接显示正文，不带标题行，结束后它作为最终回答原样留在原处。 -->
  {:else if expandable}
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
  {:else}
    <div class="conversation-phase-header conversation-phase-header--static">
      <span class="conversation-phase-marker" aria-hidden="true"></span>
      <span class="conversation-phase-label" title={summary}>{headerLabel}</span>
      {#if active}
        <span class="conversation-phase-live" aria-label={i18n.t('messageList.turnDisclosure.processing')}>
          <span class="conversation-phase-live-dot"></span>
        </span>
      {/if}
    </div>
  {/if}

  {#if expanded && expandable}
    <div
      class="conversation-phase-content"
      class:body-only={presentation.bodyOnly}
      id={contentId}
    >
      {#each details as detail (detail.entry.key)}
        <div class="turn-process-entry">
          {#if detail.kind === 'compact'}
            <ConversationProcessRow item={detail.entry.item} />
          {:else if detail.kind === 'rich'}
            <!-- 带结构的过程内容（Markdown、表格、代码、思考）按完整正文渲染，不压平成一行 -->
            <MessageItem
              message={detail.entry.item.message}
              {readOnly}
              {displayContext}
              filePreviewScope={filePreviewScopeForItem(detail.entry.item)}
              onContinueInterrupted={continueInterruptedSession}
              hideResponseDuration
            />
          {:else}
            <ConversationToolGroup
              items={detail.entry.items}
              {readOnly}
              {displayContext}
              {filePreviewScopeForItem}
              {continueInterruptedSession}
            />
          {/if}
        </div>
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

  .conversation-phase-header--static {
    cursor: default;
  }

  .conversation-phase-header--static:hover {
    color: var(--foreground-muted);
  }

  /* 与过程行同款的中性节点，保证静态标题与可展开标题左侧对齐。 */
  .conversation-phase-marker {
    position: relative;
    flex: 0 0 12px;
    width: 12px;
    height: 18px;
  }

  .conversation-phase-marker::before {
    content: '';
    position: absolute;
    top: 7px;
    left: 4px;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.52;
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

  /* 只有正文时不画过程用的左侧引导线和缩进，外观与最终回答一致。 */
  .conversation-phase-content.body-only {
    padding: 0;
    border-left: 0;
  }

  .turn-process-entry {
    min-width: 0;
  }

  /* 完整渲染的过程正文与紧凑行对齐：去掉助手消息自带的横向内边距。 */
  .turn-process-entry :global(.message-item.assistant) {
    padding-inline: 0;
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
