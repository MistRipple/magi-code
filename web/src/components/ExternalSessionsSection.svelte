<script lang="ts">
  /**
   * 工作区下的外部客户端会话（通过 Magi MCP 调用过本机工具的外部 agent，每个令牌一个会话）。
   * 它作为该工作区下的一行，与普通会话并列，用图标和「外部」标记区分；点击就是切换到这个会话，
   * 在主对话区里看它的调用过程（和内部对话同一套展示，输入区换成只读提示）。
   */
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import { formatRelativeTime } from '../lib/relative-time';
  import { externalSessions } from '../stores/external-sessions.svelte';
  import { externalApprovals } from '../stores/external-approvals.svelte';
  import type { McpExternalSession } from '../web/agent-api';

  interface Props {
    /** 只列这个工作区下的外部会话。 */
    workspaceId: string;
    currentSessionId: string;
    onOpen: (session: McpExternalSession) => void;
  }
  let { workspaceId, currentSessionId, onOpen }: Props = $props();

  let now = $state(Date.now());
  $effect(() => {
    const timer = window.setInterval(() => { now = Date.now(); }, 30_000);
    return () => window.clearInterval(timer);
  });

  const sessions = $derived(externalSessions.sessions.filter((session) => session.workspaceId === workspaceId));

  function awaitingCount(session: McpExternalSession): number {
    return externalApprovals.approvals.filter((approval) => approval.tokenId === session.tokenId).length;
  }
</script>

{#if sessions.length > 0}
  <div class="external-list" data-testid="external-sessions">
    {#each sessions as session (session.sessionId)}
      {@const awaiting = awaitingCount(session)}
      <button
        type="button"
        class="external-item"
        class:active={session.sessionId === currentSessionId}
        data-testid="external-session-item"
        data-token-id={session.tokenId}
        data-session-id={session.sessionId}
        title={session.clientName}
        onclick={() => onOpen(session)}
      >
        <span class="external-dot" class:running={session.running > 0 && awaiting === 0} class:awaiting={awaiting > 0} aria-hidden="true">
          <Icon name="plug" size={12} />
        </span>
        <span class="external-name">{session.clientName}</span>
        <span class="external-meta">
          {#if awaiting > 0}
            {i18n.t('externalSession.awaitingShort', { count: awaiting })}
          {:else}
            {i18n.t('externalSession.badge')} · {formatRelativeTime(session.lastActivityAtMs, now, i18n.locale)}
          {/if}
        </span>
      </button>
    {/each}
  </div>
{/if}

<style>
  .external-list { display: flex; flex-direction: column; gap: 1px; margin-top: 1px; }
  .external-item {
    display: flex; align-items: center; gap: var(--space-2); width: 100%; padding: 5px 10px;
    border: none; border-radius: var(--radius-md); background: transparent; color: var(--foreground);
    text-align: left; font-size: var(--text-sm); line-height: 1.35; cursor: pointer; touch-action: manipulation;
    transition: background var(--transition-fast);
  }
  .external-item:hover { background: color-mix(in srgb, var(--surface-hover) 70%, transparent); }
  .external-item.active { background: color-mix(in srgb, var(--surface-selected) 78%, transparent); font-weight: var(--font-medium); }
  .external-dot { display: inline-flex; flex: 0 0 auto; color: var(--foreground-muted); }
  .external-dot.running { color: var(--primary); animation: external-pulse 1.4s ease-in-out infinite; }
  .external-dot.awaiting { color: var(--warning); }
  .external-name { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .external-meta { flex: 0 0 auto; font-size: var(--text-xs); color: var(--foreground-muted); font-variant-numeric: tabular-nums; }
  @keyframes external-pulse { 50% { opacity: 0.35; } }
  @media (prefers-reduced-motion: reduce) { .external-dot.running { animation: none; } }
</style>
