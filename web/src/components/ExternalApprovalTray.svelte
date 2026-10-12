<script lang="ts">
  /**
   * 外部调用的全局待确认托盘：外部调用不属于用户当前打开的会话，所以不能复用会话内的授权托盘。
   * 这里是“你不在那个外部会话里时”的提醒入口；在外部会话里，同一个确认出现在输入区位置。
   * 事实与操作都在 external-approvals 存储里，托盘不保存任何状态。
   */
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { externalApprovals, trackExternalApprovals } from '../stores/external-approvals.svelte';
  import { externalSessionById } from '../stores/external-sessions.svelte';
  import { messagesState } from '../stores/messages.svelte';
  import ExternalApprovalActions from './ExternalApprovalActions.svelte';

  onMount(() => trackExternalApprovals());

  /** 正在这个外部会话里：待确认已经出现在输入区位置，托盘不重复打扰。 */
  function isViewedInSession(tokenId: string): boolean {
    return externalSessionById(messagesState.currentSessionId)?.tokenId === tokenId;
  }

  const visible = $derived(externalApprovals.approvals.filter((approval) => !isViewedInSession(approval.tokenId)));
</script>

{#if visible.length > 0}
  <div class="tray" data-external-approval-tray="1" role="alertdialog" aria-label={i18n.t('mcpServer.tray.title')}>
    <div class="tray-title">{i18n.t('mcpServer.tray.title')}</div>
    {#each visible as approval (approval.approvalId)}
      <div class="item" data-external-approval={approval.approvalId}>
        <div class="summary">{approval.summary}</div>
        <div class="meta">{i18n.t('mcpServer.approvals.from', { client: approval.clientName, prefix: approval.tokenPrefix })}</div>
        <ExternalApprovalActions {approval} />
      </div>
    {/each}
  </div>
{/if}

<style>
  .tray {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 1200;
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(360px, calc(100vw - 32px));
    padding: 12px 14px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.28));
    border-radius: 12px;
    background: var(--surface-elevated, var(--surface, #fff));
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.18);
  }

  .tray-title {
    font-size: 12px;
    font-weight: 600;
  }

  .item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
  }

  .summary {
    font-size: 12px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .meta {
    font-size: 11px;
    opacity: 0.66;
  }
</style>
