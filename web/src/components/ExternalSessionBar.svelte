<script lang="ts">
  /**
   * 外部会话的输入区位置：这个会话只读（外部客户端自己发起调用，不能在这里发起模型对话）。
   * 等待确认的调用和内部对话一样出现在会话自己的授权托盘里，这里只说明来源。
   */
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import { messagesState } from '../stores/messages.svelte';
  import { externalSessionById } from '../stores/external-sessions.svelte';

  const session = $derived(externalSessionById(messagesState.currentSessionId));
</script>

<div class="external-bar" data-testid="external-session-bar">
  <div class="external-note">
    <Icon name="plug" size={13} />
    <span>{i18n.t('externalSession.readOnlyNote', { client: session?.clientName ?? i18n.t('externalSession.title') })}</span>
  </div>
</div>

<style>
  .external-bar { max-width: 860px; width: 100%; margin: 0 auto; padding: var(--space-3) var(--space-5) var(--space-4); box-sizing: border-box; }
  .external-note { display: flex; align-items: center; gap: var(--space-2); padding: var(--space-2) var(--space-3); border: var(--magi-hairline) solid var(--border); border-radius: var(--radius-md); font-size: var(--text-xs); color: var(--foreground-muted); }
</style>
