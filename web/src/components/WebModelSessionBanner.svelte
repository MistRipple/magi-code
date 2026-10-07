<script lang="ts">
  /**
   * GPT Web 会话状态条：网页对话已失效、已保存对话冲突 / 失效、被其他会话占用、标题未同步、
   * 项目工具不可用。所有结论来自 daemon 的事实（见 `resolveWebSessionBanner`），这里只负责
   * 轮询占用者、展示与派发显式动作。
   */
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { dispatchWebModelAction } from '../web/web-model-actions';
  import { refreshWebModelRuntime } from '../web/web-model-session-projection';
  import { webModelSlotOwner } from '../stores/web-model-runtime.svelte';
  import { resolveWebSessionBanner } from '../web/web-model-session-banner';
  import type { WebConversationProjection } from '../shared/settings-bootstrap';

  interface Props {
    usesWeb: boolean;
    sessionId: string;
    projection: WebConversationProjection | null | undefined;
    turnActive: boolean;
    toolsAvailable: boolean | null;
    toolsDetail?: string;
    workspaceRequired: boolean;
    /** 发送必然被 daemon 拒绝时为 true，父组件据此禁用发送。 */
    blocksSend?: boolean;
    /** 本会话持有的槽位被释放（停止 / 退出 / 重启）：父组件据此重新拉取绑定投影。 */
    onOwnershipLost?: () => void;
  }

  let {
    usesWeb,
    sessionId,
    projection,
    turnActive,
    toolsAvailable,
    toolsDetail,
    workspaceRequired,
    blocksSend = $bindable(false),
    onOwnershipLost,
  }: Props = $props();

  let wasOwner = false;
  $effect(() => {
    const owner = webModelSlotOwner()?.sessionId ?? null;
    const isOwner = Boolean(sessionId) && owner === sessionId;
    if (wasOwner && !isOwner) onOwnershipLost?.();
    wasOwner = isOwner;
  });

  let runtimeFresh = $state(false);

  // 占用者只存在于 daemon 进程内存：turn 结束后必须重新读一次才能下"已失效"的结论。
  $effect(() => {
    if (!usesWeb) return;
    if (turnActive) {
      runtimeFresh = false;
      return;
    }
    let cancelled = false;
    void refreshWebModelRuntime().finally(() => {
      if (!cancelled) runtimeFresh = true;
    });
    return () => {
      cancelled = true;
    };
  });

  onMount(() => {
    const timer = window.setInterval(() => {
      if (usesWeb && !turnActive) void refreshWebModelRuntime();
    }, 4000);
    return () => window.clearInterval(timer);
  });

  const banner = $derived(resolveWebSessionBanner({
    usesWeb,
    projection,
    slotOwnerSessionId: webModelSlotOwner()?.sessionId ?? null,
    slotOwnerTitle: webModelSlotOwner()?.sessionTitle ?? null,
    sessionId,
    turnActive,
    runtimeFresh,
    toolsAvailable,
    toolsDetail,
    workspaceRequired,
  }));

  $effect(() => {
    blocksSend = banner?.blocksSend ?? false;
  });

  function runAction(): void {
    if (!banner?.action) return;
    dispatchWebModelAction({ kind: banner.action.kind, sessionId });
  }
</script>

{#if banner}
  <div
    class="wm-banner"
    class:warning={banner.tone === 'warning'}
    data-web-model-banner={banner.id}
    role="status"
  >
    <span class="wm-banner-text">{i18n.t(banner.textKey, banner.params)}</span>
    {#if banner.action}
      <button type="button" class="wm-banner-action" onclick={runAction}>
        {i18n.t(banner.action.labelKey)}
      </button>
    {/if}
  </div>
{/if}

<style>
  .wm-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin: 0 0 6px;
    padding: 6px 10px;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
  }

  .wm-banner.warning {
    color: var(--foreground);
    background: var(--warning-soft, rgba(220, 150, 40, 0.16));
  }

  .wm-banner-text {
    min-width: 0;
    line-height: 1.5;
  }

  .wm-banner-action {
    flex: none;
    padding: 2px 10px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.3));
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }
</style>
