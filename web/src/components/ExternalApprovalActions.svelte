<script lang="ts">
  /**
   * 外部调用待确认的操作按钮：全局托盘和外部会话里的调用卡片共用，批准与拒绝走同一个入口。
   */
  import { i18n } from '../stores/i18n.svelte';
  import type { McpServerApproval } from '../web/agent-api';
  import {
    alwaysAllowExternalApproval,
    externalApprovals,
    resolveExternalApproval,
  } from '../stores/external-approvals.svelte';

  let { approval }: { approval: McpServerApproval } = $props();

  /** GPT Web 槽位发起的调用（令牌 id 固定为 web-slot）可以切到「始终允许」。 */
  const isWebSlot = $derived(approval.tokenId === 'web-slot');
  const secondsLeft = $derived(Math.max(0, Math.ceil((approval.expiresAtMs - externalApprovals.now) / 1000)));
</script>

<div class="approval-actions" data-testid="external-approval-actions">
  <button type="button" class="btn btn--sm btn--primary" disabled={externalApprovals.busy} onclick={() => void resolveExternalApproval(approval, 'allow_once')}>
    {i18n.t('mcpServer.approvals.allow')}
  </button>
  {#if isWebSlot}
    <button type="button" class="btn btn--sm btn--secondary" disabled={externalApprovals.busy} onclick={() => void alwaysAllowExternalApproval(approval)}>
      {i18n.t('mcpServer.approvals.alwaysAllow')}
    </button>
  {/if}
  <button type="button" class="btn btn--sm btn--secondary" disabled={externalApprovals.busy} onclick={() => void resolveExternalApproval(approval, 'deny')}>
    {i18n.t('mcpServer.approvals.deny')}
  </button>
  <span class="approval-timer">{i18n.t('mcpServer.approvals.expiresIn', { seconds: secondsLeft })}</span>
</div>

<style>
  .approval-actions { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
  .approval-timer { margin-left: auto; font-size: var(--text-xs); color: var(--foreground-muted); font-variant-numeric: tabular-nums; }
</style>
