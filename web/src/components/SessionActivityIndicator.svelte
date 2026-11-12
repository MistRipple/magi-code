<script lang="ts">
  import Icon from './Icon.svelte';

  /** 侧栏会话前置指示的唯一实现：所有会话列表共用同一个槽位和同一套状态表现。 */
  type SessionActivityIndicatorState = 'running' | 'unread' | 'awaiting' | 'none';

  interface Props {
    state: SessionActivityIndicatorState;
    title?: string;
  }

  let { state, title }: Props = $props();
</script>

<span
  class="session-activity-indicator {state}"
  data-activity-state={state}
  title={title || undefined}
  aria-hidden="true"
>
  {#if state === 'running'}<Icon name="loader" size={12} />{/if}
</span>

<style>
  /* 固定 14px 槽位：状态切换不挪动会话名称。 */
  .session-activity-indicator {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 14px;
    width: 14px;
    height: 14px;
  }

  /* 运行中只用匀速旋转表达，不叠加呼吸或光晕，避免动效过多而不清晰。 */
  .session-activity-indicator.running {
    color: var(--info);
  }

  .session-activity-indicator.running :global(svg) {
    animation: magi-status-spin 1.2s linear infinite;
  }

  /* 静态状态只用颜色区分，没有动画。 */
  .session-activity-indicator.unread::before,
  .session-activity-indicator.awaiting::before {
    content: '';
    width: 6px;
    height: 6px;
    border-radius: var(--radius-full);
  }

  .session-activity-indicator.unread::before {
    background: var(--success);
    box-shadow: 0 0 7px color-mix(in srgb, var(--success) 42%, transparent);
  }

  .session-activity-indicator.awaiting::before {
    background: var(--warning);
    box-shadow: 0 0 7px color-mix(in srgb, var(--warning) 42%, transparent);
  }

  /* 小尺寸进度旋转是“仍在运行”的唯一动态线索，系统开启减少动态效果时也保留；
     全局规则会把动画压成一次 0.01ms，这里显式恢复持续旋转。 */
  @media (prefers-reduced-motion: reduce) {
    .session-activity-indicator.running :global(svg) {
      animation-duration: 1.2s !important;
      animation-iteration-count: infinite !important;
    }
  }
</style>
