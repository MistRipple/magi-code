<script lang="ts">
  /**
   * 「Magi MCP 服务 → 活动」里的调用记录：分页浏览，可按客户端过滤，可清理。
   *
   * 记录由 daemon 保存并分页返回，这里不缓存、不落任何浏览器存储。清理只影响回看用的记录，
   * 不影响令牌、授权或变更账本。
   */
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import { addToast } from '../stores/messages.svelte';
  import {
    clearMcpServerAudit,
    listMcpServerAudit,
    type McpServerAuditEntry,
    type McpServerToken,
  } from '../web/agent-api';

  const PAGE_SIZE = 20;
  const REFRESH_MS = 5000;

  let { tokens, version = 0 }: { tokens: McpServerToken[]; version?: number } = $props();

  let entries = $state<McpServerAuditEntry[]>([]);
  let total = $state(0);
  let offset = $state(0);
  let filterTokenId = $state('');
  let busy = $state(false);

  const pageCount = $derived(Math.max(1, Math.ceil(total / PAGE_SIZE)));
  const pageIndex = $derived(Math.floor(offset / PAGE_SIZE) + 1);
  const filterName = $derived(
    tokens.find((token) => token.tokenId === filterTokenId)?.clientName ?? '',
  );

  function messageOf(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  async function load(): Promise<void> {
    try {
      const page = await listMcpServerAudit({
        limit: PAGE_SIZE,
        offset,
        tokenId: filterTokenId || undefined,
      });
      total = page.total;
      // 清理或过滤后当前页可能已经不存在：回到最后一页。
      if (page.entries.length === 0 && page.total > 0 && offset > 0) {
        offset = Math.floor((page.total - 1) / PAGE_SIZE) * PAGE_SIZE;
        return load();
      }
      entries = page.entries;
    } catch {
      /* 下一次刷新再试 */
    }
  }

  function go(delta: number): void {
    offset = Math.max(0, Math.min((pageCount - 1) * PAGE_SIZE, offset + delta * PAGE_SIZE));
    void load();
  }

  function changeFilter(): void {
    offset = 0;
    void load();
  }

  async function clearRecords(): Promise<void> {
    const message = filterTokenId
      ? i18n.t('mcpServer.audit.clearClientConfirm', { client: filterName, count: String(total) })
      : i18n.t('mcpServer.audit.clearAllConfirm', { count: String(total) });
    if (total === 0 || !(await confirmMessage(message, { tone: 'danger' }))) return;
    busy = true;
    try {
      const result = await clearMcpServerAudit(filterTokenId || undefined);
      addToast('success', i18n.t('mcpServer.audit.cleared', { count: String(result.removed) }));
      offset = 0;
      await load();
    } catch (error) {
      addToast('error', i18n.t('mcpServer.error.action', { message: messageOf(error) }));
    } finally {
      busy = false;
    }
  }

  /** 管理类事件（查看原文、编辑、重新生成）显示可读名称；工具调用保持原名。 */
  const MANAGEMENT_ACTIONS = ['token.view_secret', 'token.update', 'token.rotate'];
  function actionLabel(tool: string): string {
    return MANAGEMENT_ACTIONS.includes(tool) ? i18n.t(`mcpServer.audit.action.${tool}`) : tool;
  }

  function formatTime(ms: number): string {
    return new Date(ms).toLocaleString();
  }

  // 审批处理等外部变化通知刷新。
  $effect(() => {
    void version;
    void load();
  });

  onMount(() => {
    const timer = window.setInterval(() => void load(), REFRESH_MS);
    return () => window.clearInterval(timer);
  });
</script>

<div class="card" data-mcp-server-audit="1">
  <div class="audit-head">
    <h4>{i18n.t('mcpServer.audit.section')}</h4>
    <span class="meta" data-mcp-audit-total="1">{i18n.t('mcpServer.audit.total', { count: String(total) })}</span>
  </div>

  <div class="audit-tools">
    <label class="field audit-filter">
      <span>{i18n.t('mcpServer.audit.filter')}</span>
      <select bind:value={filterTokenId} onchange={changeFilter} data-mcp-audit-filter="1">
        <option value="">{i18n.t('mcpServer.audit.filterAll')}</option>
        {#each tokens as token (token.tokenId)}
          <option value={token.tokenId}>{token.clientName} · {token.prefix}…</option>
        {/each}
      </select>
    </label>
    <div class="action-row">
      <button type="button" disabled={busy} onclick={() => void load()}>{i18n.t('mcpServer.audit.refresh')}</button>
      <button type="button" class="danger" data-mcp-audit-clear="1" disabled={busy || total === 0} onclick={() => void clearRecords()}>
        {filterTokenId ? i18n.t('mcpServer.audit.clearClient') : i18n.t('mcpServer.audit.clearAll')}
      </button>
    </div>
  </div>

  {#if entries.length === 0}
    <p class="meta">{i18n.t('mcpServer.audit.empty')}</p>
  {:else}
    <ul class="rows">
      {#each entries as entry, index (`${entry.atMs}-${entry.tokenId}-${offset + index}`)}
        <li>
          <div class="row-main">
            <span class="row-title">
              <code>{actionLabel(entry.tool)}</code>
              <span class="badge" class:badge--off={entry.outcome !== 'succeeded'}>
                {i18n.t(`mcpServer.audit.outcome.${entry.outcome}`)}
              </span>
            </span>
            <span class="meta">
              {entry.clientName} · {formatTime(entry.atMs)}
              {#if entry.paths.length > 0} · {entry.paths.slice(0, 3).join(', ')}{/if}
              {#if entry.detail} · {entry.detail}{/if}
            </span>
          </div>
        </li>
      {/each}
    </ul>
    <div class="pager" data-mcp-audit-pager="1">
      <button type="button" disabled={offset === 0} onclick={() => go(-1)}>{i18n.t('mcpServer.audit.prev')}</button>
      <span class="meta">{i18n.t('mcpServer.audit.page', { page: String(pageIndex), pages: String(pageCount) })}</span>
      <button type="button" disabled={pageIndex >= pageCount} onclick={() => go(1)}>{i18n.t('mcpServer.audit.next')}</button>
    </div>
  {/if}
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
    border-radius: 10px;
  }

  .audit-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }

  h4 {
    margin: 0;
    font-size: 13px;
  }

  .audit-tools {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    justify-content: space-between;
    gap: 8px;
  }

  .audit-filter {
    min-width: 180px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
  }

  .field select {
    padding: 4px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 6px;
    font-size: 12px;
    background: transparent;
    color: inherit;
  }

  .meta {
    margin: 0;
    font-size: 11px;
    opacity: 0.66;
    overflow-wrap: anywhere;
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
  }

  .rows li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
  }

  .row-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .row-title {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
  }

  .badge {
    padding: 1px 6px;
    border-radius: 999px;
    font-size: 10px;
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    color: var(--accent-strong, #3b6fd4);
  }

  .badge--off {
    background: var(--surface-muted, rgba(127, 127, 127, 0.14));
    color: inherit;
    opacity: 0.8;
  }

  .pager {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding-top: 4px;
  }

  .action-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
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

  button.danger {
    color: var(--danger, #c53f4f);
  }

  button:disabled {
    cursor: default;
    opacity: 0.45;
  }
</style>
