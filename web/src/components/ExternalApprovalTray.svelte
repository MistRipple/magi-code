<script lang="ts">
  /**
   * 外部 MCP 客户端的全局待确认托盘。
   *
   * 外部调用不属于用户当前打开的会话，所以不能复用会话内的授权托盘。事实在 daemon：
   * 这里只在收到 `tool.approval.requested`（external）事件时拉取一次，并且**只有存在待确认项时**
   * 才做低频轮询（用来跟随超时与别处处理的结果）；没有待确认项时不产生任何周期请求。
   * 远程访问（经公网隧道）没有管理权限，首次请求失败后托盘自行停用。
   */
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { addToast } from '../stores/messages.svelte';
  import {
    listMcpServerApprovals,
    setWebModelToolPolicy,
    resolveMcpServerApproval,
    type McpServerApproval,
  } from '../web/agent-api';
  import { MCP_APPROVALS_CHANGED_EVENT } from '../lib/mcp-server-events';

  const POLL_WHILE_PENDING_MS = 3000;

  let approvals = $state<McpServerApproval[]>([]);
  let now = $state(Date.now());
  let busy = $state(false);
  let disabled = false;

  async function refresh(): Promise<void> {
    if (disabled) return;
    try {
      approvals = await listMcpServerApprovals();
    } catch {
      // 没有管理权限（例如经隧道访问）或 daemon 暂不可用：停用，不反复重试。
      disabled = true;
      approvals = [];
    }
  }

  async function resolve(approval: McpServerApproval, decision: 'allow_once' | 'deny'): Promise<void> {
    busy = true;
    try {
      approvals = await resolveMcpServerApproval(approval.approvalId, decision);
    } catch (error) {
      addToast('error', i18n.t('mcpServer.error.action', {
        message: error instanceof Error ? error.message : String(error),
      }));
      await refresh();
    } finally {
      busy = false;
    }
  }

  /** GPT Web 槽位发起的调用（令牌 id 固定为 web-slot）。 */
  function isWebSlotApproval(approval: McpServerApproval): boolean {
    return approval.tokenId === 'web-slot';
  }

  /** 「始终允许」：把 GPT Web 的授权方式切到始终授权，并放行这一次。随时可在设置里改回。 */
  async function alwaysAllow(approval: McpServerApproval): Promise<void> {
    busy = true;
    try {
      await setWebModelToolPolicy({ approvalMode: 'always' });
      approvals = await resolveMcpServerApproval(approval.approvalId, 'allow_once');
      addToast('info', i18n.t('mcpServer.approvals.alwaysSet'));
    } catch (error) {
      addToast('error', i18n.t('mcpServer.error.action', {
        message: error instanceof Error ? error.message : String(error),
      }));
      await refresh();
    } finally {
      busy = false;
    }
  }

  function secondsLeft(approval: McpServerApproval): number {
    return Math.max(0, Math.ceil((approval.expiresAtMs - now) / 1000));
  }

  onMount(() => {
    void refresh();
    const onChanged = () => { void refresh(); };
    window.addEventListener(MCP_APPROVALS_CHANGED_EVENT, onChanged);
    const clock = window.setInterval(() => { now = Date.now(); }, 1000);
    // 只有存在待确认项时才轮询（跟随超时与别处处理的结果）。
    const pollTimer = window.setInterval(() => {
      if (approvals.length > 0) void refresh();
    }, POLL_WHILE_PENDING_MS);
    return () => {
      window.removeEventListener(MCP_APPROVALS_CHANGED_EVENT, onChanged);
      window.clearInterval(clock);
      window.clearInterval(pollTimer);
    };
  });
</script>

{#if approvals.length > 0}
  <div class="tray" data-external-approval-tray="1" role="alertdialog" aria-label={i18n.t('mcpServer.tray.title')}>
    <div class="tray-title">{i18n.t('mcpServer.tray.title')}</div>
    {#each approvals as approval (approval.approvalId)}
      <div class="item" data-external-approval={approval.approvalId}>
        <div class="summary">{approval.summary}</div>
        <div class="meta">
          {i18n.t('mcpServer.approvals.from', { client: approval.clientName, prefix: approval.tokenPrefix })}
          · {i18n.t('mcpServer.approvals.expiresIn', { seconds: secondsLeft(approval) })}
        </div>
        <div class="actions">
          <button type="button" class="primary" disabled={busy} onclick={() => void resolve(approval, 'allow_once')}>
            {i18n.t('mcpServer.approvals.allow')}
          </button>
          {#if isWebSlotApproval(approval)}
            <button type="button" disabled={busy} onclick={() => void alwaysAllow(approval)}>
              {i18n.t('mcpServer.approvals.alwaysAllow')}
            </button>
          {/if}
          <button type="button" disabled={busy} onclick={() => void resolve(approval, 'deny')}>
            {i18n.t('mcpServer.approvals.deny')}
          </button>
        </div>
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

  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }

  button {
    padding: 5px 12px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 8px;
    font-size: 12px;
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  button.primary {
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    border-color: transparent;
    color: var(--accent-strong, #3b6fd4);
  }

  button:disabled {
    cursor: default;
    opacity: 0.45;
  }
</style>
