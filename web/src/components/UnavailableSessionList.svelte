<script lang="ts">
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { AgentUnavailableSession } from '../web/agent-api';

  interface Props {
    sessions: AgentUnavailableSession[];
    onDelete: (session: AgentUnavailableSession) => void;
  }

  let { sessions, onDelete }: Props = $props();
</script>

{#if sessions.length > 0}
  <ul class="unavailable-sessions" aria-label={i18n.t('web.unavailableSession')}>
    {#each sessions as session (session.sessionId)}
      <li
        class="unavailable-session"
        title={i18n.t('web.unavailableSessionDetail', {
          path: session.sourcePath,
          reason: session.reason,
        })}
      >
        <Icon name="warning" size={12} class="unavailable-session-icon" />
        <span class="unavailable-session-label">{i18n.t('web.unavailableSession')}</span>
        <span class="unavailable-session-id">{session.sessionId}</span>
        <button
          type="button"
          class="btn-icon btn-icon--sm btn-icon--danger unavailable-session-delete"
          title={i18n.t('header.deleteSession')}
          aria-label={i18n.t('header.deleteSession')}
          onclick={() => onDelete(session)}
        >
          <Icon name="delete" size={12} />
        </button>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .unavailable-sessions {
    list-style: none;
    margin: 0;
    padding: 0 0 0 var(--space-2);
  }

  .unavailable-session {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    padding: var(--space-1) var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  /* 关键补救操作不依赖 hover：始终可见，避免触屏和键盘用户找不到入口。 */
  .unavailable-session-delete {
    margin-inline-start: auto;
    flex-shrink: 0;
  }

  .unavailable-session :global(.unavailable-session-icon) {
    flex-shrink: 0;
    color: var(--warning);
  }

  .unavailable-session-label {
    flex-shrink: 0;
  }

  .unavailable-session-id {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    opacity: 0.7;
  }
</style>
