<script lang="ts">
  import SelectField from './SelectField.svelte';
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
  import { closeSettings } from '../stores/shell-ui.svelte';
  import { externalSessions, openExternalSession, refreshExternalSessions } from '../stores/external-sessions.svelte';
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

  /** 选中的客户端有外部会话时，可以直接去看它的调用过程。 */
  const hasSession = $derived(
    filterTokenId !== '' && externalSessions.sessions.some((session) => session.tokenId === filterTokenId),
  );

  function viewSession(): void {
    const session = externalSessions.sessions.find((item) => item.tokenId === filterTokenId);
    if (!session) return;
    closeSettings();
    openExternalSession(session);
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
    if (total === 0 || !(await confirmMessage(message, { tone: 'danger', confirmLabel: i18n.t(filterTokenId ? 'mcpServer.audit.clearClient' : 'mcpServer.audit.clearAll') }))) return;
    busy = true;
    try {
      const result = await clearMcpServerAudit(filterTokenId || undefined);
      void refreshExternalSessions();
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
    return new Date(ms).toLocaleString(i18n.locale);
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

<div class="settings-card mcp-card" data-mcp-server-audit="1">
  <div class="audit-head">
    <h4>{i18n.t('mcpServer.audit.section')}</h4>
    <span class="meta" data-mcp-audit-total="1">{i18n.t('mcpServer.audit.total', { count: String(total) })}</span>
  </div>

  <p class="meta">{i18n.t('mcpServer.audit.hint')}</p>

  <div class="audit-tools">
    <div class="field audit-filter">
      <span>{i18n.t('mcpServer.audit.filter')}</span>
      <SelectField bind:value={filterTokenId} onchange={(next) => { filterTokenId = next; changeFilter(); }} data-mcp-audit-filter="1"
        label={i18n.t('mcpServer.audit.filter')}
        options={[{ value: '', label: i18n.t('mcpServer.audit.filterAll') }, ...tokens.map(token => ({ value: token.tokenId, label: `${token.clientName}: ${token.prefix}…` }))]} />
    </div>
    <div class="action-row">
      {#if hasSession}
        <button class="btn btn--sm btn--secondary" type="button" data-mcp-audit-view-session="1" onclick={viewSession}>{i18n.t('mcpServer.audit.viewSession')}</button>
      {/if}
      <button class="btn btn--sm btn--secondary" type="button" disabled={busy} onclick={() => void load()}>{i18n.t('mcpServer.audit.refresh')}</button>
      <button type="button" class="btn btn--sm btn--error" data-mcp-audit-clear="1" disabled={busy || total === 0} onclick={() => void clearRecords()}>
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
          <div class="settings-row-copy">
            <span class="settings-row-title">
              <code>{actionLabel(entry.tool)}</code>
              <span class="settings-chip settings-chip--ok" class:badge--off={entry.outcome !== 'succeeded'}>
                {i18n.t(`mcpServer.audit.outcome.${entry.outcome}`)}
              </span>
            </span>
            <span class="meta ui-meta">
              <span>{entry.clientName}</span><span>{formatTime(entry.atMs)}</span>
              {#if entry.paths.length > 0}<span>{entry.paths.slice(0, 3).join(', ')}</span>{/if}
              {#if entry.detail}<span>{entry.detail}</span>{/if}
            </span>
          </div>
        </li>
      {/each}
    </ul>
    <div class="pager" data-mcp-audit-pager="1">
      <button class="btn btn--sm btn--secondary" type="button" disabled={offset === 0} onclick={() => go(-1)}>{i18n.t('mcpServer.audit.prev')}</button>
      <span class="meta">{i18n.t('mcpServer.audit.page', { page: String(pageIndex), pages: String(pageCount) })}</span>
      <button class="btn btn--sm btn--secondary" type="button" disabled={pageIndex >= pageCount} onclick={() => go(1)}>{i18n.t('mcpServer.audit.next')}</button>
    </div>
  {/if}
</div>

<style>
  .mcp-card {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: var(--space-3);
    padding: var(--space-4);
  }

  .audit-head {
    display: flex;
    flex-wrap: wrap;
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
    flex: 1 1 180px;
    min-width: min(180px, 100%);
    max-width: 100%;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    font-size: var(--text-xs);
  }

  .meta {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 0;
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
  }

  .rows li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-width: 0;
    padding: var(--space-3) 0;
  }

  .rows li + li { border-top: var(--magi-hairline) solid var(--border-subtle); }
  .rows .settings-row-copy { overflow-wrap: anywhere; }
  .badge--off { color: var(--foreground-muted); }

  .pager {
    display: flex;
    flex-wrap: wrap;
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

  @container mcp-server (max-width: 420px) {
    .mcp-card { padding: var(--space-3); }
  }

</style>
