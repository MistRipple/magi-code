<script lang="ts">
  import { untrack } from 'svelte';
  import Disclosure from './Disclosure.svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import {
    resolveConversationPhaseDetails,
    resolveConversationPhasePresentation,
    resolveConversationPhaseSummary,
    type ConversationPhase as ConversationPhaseModel,
  } from '../lib/conversation-disclosure';
  import { i18n } from '../stores/i18n.svelte';
  import ToolCardMeta from './ToolCardMeta.svelte';
  import MessageItem from './MessageItem.svelte';
  import ConversationProcessRow from './ConversationProcessRow.svelte';
  import ConversationThinkingText from './ConversationThinkingText.svelte';
  import { resolveThinkingTitle } from '../lib/thinking-title';
  import ConversationToolGroup from './ConversationToolGroup.svelte';

  interface Props {
    phase: ConversationPhaseModel;
    active?: boolean;
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
  }

  let {
    phase,
    active = false,
    readOnly = false,
    displayContext = 'thread',
    filePreviewScopeForItem,
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
  const firstThinkingGroup = $derived.by(() => {
    const first = phase.entries[0];
    if (first?.kind !== 'event') return undefined;
    return first.item.message.blocks?.find((block) => block?.type === 'thinking')?.thinking;
  });
  const headerLabel = $derived.by(() => {
    // 独立的思考行：标题始终是思考自己的状态（思考中 / 思考已完成），不用内容预览——
    // 否则收起后只剩一句看不出是「思考」的话。
    // 模型已经转去输出文字 / 调用工具（这一阶段不再是当前阶段）时，思考就是结束了，
    // 即使投影里的思考项要等整轮结束才标记完成。
    if (presentation.thinkingOnly) {
      return resolveThinkingTitle(
        firstThinkingGroup && { ...firstThinkingGroup, isStreaming: firstThinkingGroup.isStreaming && active },
        i18n.t.bind(i18n),
      );
    }
    if (!presentation.headerRepeatsBody) return summary;
    return i18n.t(active ? 'messageList.turnDisclosure.processing' : 'messageList.turnDisclosure.processed');
  });

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
      <span class="conversation-phase-label" title={summary}>{headerLabel}</span>
      <ToolCardMeta status={active ? 'running' : undefined} label={headerLabel} {expanded} />
    </button>
  {:else}
    <div class="conversation-phase-header conversation-phase-header--static">
      <span class="conversation-phase-label" title={summary}>{headerLabel}</span>
      <ToolCardMeta status={active ? 'running' : undefined} label={headerLabel} />
    </div>
  {/if}

  <Disclosure expanded={expanded && expandable}>
    <div
      class="conversation-phase-content"
      class:body-only={presentation.bodyOnly}
      id={contentId}
    >
      {#each details as detail (detail.entry.key)}
        <div class="turn-process-entry">
          {#if detail.kind === 'compact'}
            <ConversationProcessRow item={detail.entry.item} />
          {:else if detail.kind === 'rich' && detail.entry.item.message.type === 'thinking'}
            <!-- 思考：摘要模式只缩进显示思考文字，状态由阶段标题表达，不再嵌一张详细模式的思考卡片 -->
            <ConversationThinkingText
              message={detail.entry.item.message}
              filePreviewScope={filePreviewScopeForItem(detail.entry.item)}
            />
          {:else if detail.kind === 'rich'}
            <!-- 带结构的过程内容（Markdown、表格、代码）按完整正文渲染，不压平成一行 -->
            <MessageItem
              message={detail.entry.item.message}
              {readOnly}
              {displayContext}
              filePreviewScope={filePreviewScopeForItem(detail.entry.item)}
              hideResponseDuration
            />
          {:else}
            <ConversationToolGroup
              items={detail.entry.items}
              {readOnly}
              {displayContext}
              {filePreviewScopeForItem}
            />
          {/if}
        </div>
      {/each}
    </div>
  </Disclosure>
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
    padding: 0 var(--tool-card-padding);
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

  .conversation-phase-label {
    min-width: 0;
    overflow: hidden;
    color: currentColor;
    font-size: var(--text-sm);
    line-height: 1.3;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .conversation-phase-content {
    display: flex;
    flex-direction: column;
    gap: 0;
    min-width: 0;
    padding: 0 0 1px 8px;
    border-left: var(--magi-hairline) solid color-mix(in srgb, var(--border) 74%, transparent);
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

  @media (max-width: 560px) {
    .conversation-phase-header {
      min-height: 32px;
    }

    .conversation-phase-content {
      padding-left: 6px;
    }
  }
</style>
