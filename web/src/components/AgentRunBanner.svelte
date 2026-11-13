<script lang="ts">
  /**
   * 对话里的代理运行恢复横幅。
   *
   * 只在这一轮运行需要用户处理时出现（可恢复、失败、降级、被终止，或主线正在处理子代理失败），
   * 操作完全由后端 `availableActions` 驱动：继续在输入框的发送按钮上，横幅只有归档。
   * 位置与待确认的工具审批相同：在消息列表下方、输入区上方，不遮挡正在看的消息。
   */
  import Icon from './Icon.svelte';
  import { resolveAgentRunNotice } from '../lib/active-agent-center';
  import { activeAgentRun } from '../stores/active-agent-run.svelte';
  import { i18n } from '../stores/i18n.svelte';

  const outcome = $derived(activeAgentRun.outcome);
  const failureSummary = $derived(activeAgentRun.failureSummary);
  const continuation = $derived(activeAgentRun.continuation);
  const notice = $derived(resolveAgentRunNotice({
    outcome,
    hasAgentEntries: activeAgentRun.hasAgentEntries,
    failedAgentCount: failureSummary?.failedAgentCount ?? 0,
    availableActions: activeAgentRun.availableActions,
  }));

</script>

{#if notice}
  <div class="run-banner-wrap">
  <section class="run-banner run-banner--{notice.tone}" aria-live="polite" data-testid="agent-run-banner">
    <span class="run-banner-icon" aria-hidden="true">
      <Icon name={outcome === 'active' ? 'alert-triangle' : 'warning'} size={15} />
    </span>
    <span class="run-banner-copy">
      <strong>{i18n.t(`activeAgentCenter.recovery.${notice.kind}`)}</strong>
      <span>{i18n.t(`activeAgentCenter.recovery.${notice.kind}Hint`)}</span>
      {#if continuation}<span class="run-banner-hint">{i18n.t(`activeAgentCenter.recovery.${continuation}`)}</span>{/if}
      {#if activeAgentRun.actionError}<code class="run-banner-error">{activeAgentRun.actionError}</code>{/if}
    </span>
    {#if notice.showAgents || notice.canArchive}
    <span class="run-banner-actions">
      {#if notice.showAgents}
        <button type="button" class="btn btn--secondary btn--sm run-banner-view" onclick={() => activeAgentRun.openList()}>
          <Icon name="bot" size={12} />
          {i18n.t('activeAgentCenter.viewAgents')}
        </button>
      {/if}
      {#if notice.canArchive}
        <button
          type="button"
          class="btn btn--secondary btn--sm run-banner-action--archive"
          disabled={activeAgentRun.actionInFlight !== null}
          onclick={() => void activeAgentRun.runAction('archive')}
        >
          <Icon
            name={activeAgentRun.actionInFlight === 'archive' ? 'loader' : 'eye-slash'}
            size={12}
            class={activeAgentRun.actionInFlight === 'archive' ? 'spinning' : ''}
          />
          {i18n.t('activeAgentCenter.action.archive')}
        </button>
      {/if}
    </span>
    {/if}
  </section>
  </div>
{/if}

<style>
  /* 与待确认的工具审批同一套外框：外层限宽并留出左右边距，内层才是带边框的卡片。 */
  .run-banner-wrap {
    box-sizing: border-box;
    flex: 0 0 auto;
    align-self: center;
    width: min(100%, var(--conversation-content-width, 100%));
    padding: var(--space-2) var(--space-4) var(--space-3);
  }

  .run-banner {
    display: flex;
    align-items: flex-start;
    gap: var(--space-4);
    box-sizing: border-box;
    padding: var(--space-3) var(--space-4);
    border: var(--magi-hairline) solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
  }

  .run-banner--error {
    border-color: color-mix(in srgb, var(--error) 40%, var(--border));
    background: color-mix(in srgb, var(--error) 8%, transparent);
  }

  .run-banner--warning {
    border-color: color-mix(in srgb, var(--warning) 40%, var(--border));
    background: color-mix(in srgb, var(--warning) 8%, transparent);
  }

  .run-banner-icon {
    display: inline-flex;
    flex: 0 0 auto;
    padding-top: 1px;
    color: var(--foreground-muted);
  }

  .run-banner--error .run-banner-icon { color: var(--error); }
  .run-banner--warning .run-banner-icon { color: var(--warning); }

  .run-banner-copy {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    font-size: var(--text-sm);
  }

  .run-banner-copy span {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .run-banner-copy code {
    overflow-wrap: anywhere;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .run-banner-error {
    color: var(--error);
  }

  .run-banner-actions {
    display: flex;
    flex: 0 0 auto;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-3);
  }

  /* 窄屏纵向空间紧：说明文字收起，只留标题与真实错误。 */
  @media (max-width: 640px) {
    .run-banner {
      flex-wrap: wrap;
    }

    .run-banner-copy > span:not(.run-banner-error):not(.run-banner-hint) {
      display: none;
    }

    .run-banner-actions {
      flex-basis: 100%;
      justify-content: flex-start;
    }
  }
</style>
