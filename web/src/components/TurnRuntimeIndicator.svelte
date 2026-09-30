<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { formatElapsed } from '../lib/utils';
  import { messagesState } from '../stores/messages.svelte';
  import { webModelTurnStage } from '../stores/web-model-runtime.svelte';
  import { interruptAgentSession } from '../web/agent-api';

  interface Props {
    elapsedSeconds: number;
    /**
     * 承载该运行指示的会话 id。
     *
     * 不传时用当前会话——Web 阶段投影只对「这个会话正在跑 Web 引擎」有意义，
     * 拿不到归属就不显示专属阶段，宁可少一行也不张冠李戴。
     */
    sessionId?: string;
  }

  let { elapsedSeconds, sessionId }: Props = $props();
  const elapsedLabel = $derived(formatElapsed(Math.max(0, elapsedSeconds)));

  /**
   * daemon 投影的 Web turn 阶段（《实现计划》§3.12）。
   *
   * 「等待 Web 引擎」与「排队中 · 第 N 位」都锚在同一个 `waiting_engine` 上：
   * 队列位置由 daemon 按 FIFO 给出，前端只做展示，不自行推断位置。
   */
  const ownerSessionId = $derived(sessionId?.trim() || messagesState.currentSessionId || '');
  const webStage = $derived(
    ownerSessionId ? webModelTurnStage(ownerSessionId) : null,
  );
  const stageLabel = $derived.by(() => {
    if (!webStage) return '';
    if (webStage.stage === 'generating') {
      return i18n.t('webModel.turnStage.generating');
    }
    const position = webStage.queuePosition ?? 0;
    if (position > 1) {
      return i18n.t('webModel.turnStage.queued', { position });
    }
    return i18n.t('webModel.turnStage.waitingEngine');
  });
  const queued = $derived(
    Boolean(webStage) && webStage?.stage === 'waiting_engine' && (webStage.queuePosition ?? 0) > 1,
  );
  let cancelling = $state(false);

  /**
   * 排队期间必须能取消（§9.3 #16）：取消即出队，不留「还在排队」的残留状态。
   * 走既有的会话中断入口，不新增第二条取消路径。
   */
  async function cancelQueuedTurn(): Promise<void> {
    if (cancelling || !ownerSessionId) return;
    cancelling = true;
    try {
      await interruptAgentSession(ownerSessionId);
    } catch (error) {
      console.warn('[TurnRuntimeIndicator] 取消排队失败:', error);
    } finally {
      cancelling = false;
    }
  }
</script>

<div class="turn-runtime-indicator" aria-label={i18n.t('runtimeState.status.running')} role="status">
  <span class="turn-runtime-dot"></span>
  <span class="turn-runtime-dot"></span>
  <span class="turn-runtime-dot"></span>
  {#if stageLabel}
    <span class="turn-runtime-stage" data-web-model-stage={webStage?.stage}>{stageLabel}</span>
  {/if}
  <span class="turn-runtime-elapsed-time">{elapsedLabel}</span>
  {#if queued}
    <button
      type="button"
      class="turn-runtime-cancel"
      data-web-model-cancel-queued="1"
      disabled={cancelling}
      onclick={() => void cancelQueuedTurn()}
    >{i18n.t('webModel.action.cancelQueued')}</button>
  {/if}
</div>

<style>
  .turn-runtime-indicator {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 24px;
    margin-top: var(--space-2);
    padding: var(--space-1) 0;
    color: var(--foreground-muted);
  }

  .turn-runtime-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--foreground-muted);
    opacity: 0.72;
    animation: turnRuntimeBounce 1.4s ease-in-out infinite;
  }

  .turn-runtime-dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .turn-runtime-dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  .turn-runtime-stage {
    margin-left: var(--space-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .turn-runtime-cancel {
    margin-left: var(--space-2);
    padding: 1px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .turn-runtime-cancel:disabled {
    cursor: default;
    opacity: 0.5;
  }

  .turn-runtime-elapsed-time {
    margin-left: var(--space-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }

  @keyframes turnRuntimeBounce {
    0%, 80%, 100% {
      opacity: 0.45;
      transform: translateY(0);
    }
    40% {
      opacity: 1;
      transform: translateY(-3px);
    }
  }
</style>
