<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { formatElapsed } from '../lib/utils';
  import { messagesState } from '../stores/messages.svelte';
  import { webModelTurnStage } from '../stores/web-model-runtime.svelte';

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
   * daemon 投影的 GPT Web turn 阶段。单槽位没有排队：槽位被占用时发送直接被拒绝，
   * 所以这里只有「正在生成」一种专属阶段。
   */
  const ownerSessionId = $derived(sessionId?.trim() || messagesState.currentSessionId || '');
  const webStage = $derived(
    ownerSessionId ? webModelTurnStage(ownerSessionId) : null,
  );
  const stageLabel = $derived(webStage ? i18n.t('webModel.turnStage.generating') : '');
</script>

<div class="turn-runtime-indicator" aria-label={i18n.t('runtimeState.status.running')} role="status">
  <span class="turn-runtime-dot"></span>
  <span class="turn-runtime-dot"></span>
  <span class="turn-runtime-dot"></span>
  {#if stageLabel}
    <span class="turn-runtime-stage" data-web-model-stage={webStage?.stage}>{stageLabel}</span>
  {/if}
  <span class="turn-runtime-elapsed-time">{elapsedLabel}</span>
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
