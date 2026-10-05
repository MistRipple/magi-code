<script lang="ts">
  /**
   * Magi 代理在内置浏览器中的可视光标。
   *
   * `<webview>` 的 guest 是独立的原生合成层，普通 z-index 盖不住它，所以覆盖层
   * 使用 Popover Top Layer 并锚定到内容槽，尺寸与内容槽一致、整体不接收指针事件：
   * 用户在页面上的任何操作都直接落到网页，并照常触发接管。
   */
  import { onDestroy } from 'svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import {
    AGENT_CURSOR_ACTION_HOLD_MILLIS,
    AGENT_CURSOR_IDLE_MILLIS,
    agentCursorActionLabelKey,
    agentCursorGlideMillis,
    agentCursorTagPlacement,
    isStationaryMove,
    type AgentCursorAction,
    type AgentCursorPoint,
  } from '../../lib/agent-cursor';

  export interface AgentCursorSignal {
    /** 每条 agent_cursor 事件递增，同一位置的连续点击也能重新触发反馈。 */
    sequence: number;
    visible: boolean;
    point: AgentCursorPoint | null;
    action: AgentCursorAction | null;
  }

  interface Props {
    anchorName: string;
    signal: AgentCursorSignal | null;
    /** 宿主不可见（折叠、隐藏或后台离屏挂载）时不显示覆盖层。 */
    hostVisible: boolean;
  }

  let { anchorName, signal, hostVisible }: Props = $props();

  const LEAVE_MILLIS = 180;
  const PRESS_MILLIS = 140;
  const RIPPLE_MILLIS = 560;

  let layer = $state<HTMLDivElement | undefined>();
  let layerWidth = $state(0);
  let layerHeight = $state(0);
  let point = $state<AgentCursorPoint | null>(null);
  let glideMillis = $state(0);
  let shown = $state(false);
  let leaving = $state(false);
  let entering = $state(false);
  let pressing = $state(false);
  let idle = $state(false);
  let labelAction = $state<AgentCursorAction | null>(null);
  let ripples = $state<number[]>([]);

  let lastSequence = -1;
  let rippleSequence = 0;
  const timers = new Set<ReturnType<typeof setTimeout>>();
  let labelTimer: ReturnType<typeof setTimeout> | null = null;
  let idleTimer: ReturnType<typeof setTimeout> | null = null;
  let leaveTimer: ReturnType<typeof setTimeout> | null = null;

  const active = $derived(shown && hostVisible);
  const placement = $derived(
    point ? agentCursorTagPlacement(point, { width: layerWidth, height: layerHeight }) : { flipX: false, flipY: false },
  );
  const labelKey = $derived(agentCursorActionLabelKey(labelAction));

  function later(callback: () => void, millis: number): ReturnType<typeof setTimeout> {
    const timer = setTimeout(() => {
      timers.delete(timer);
      callback();
    }, millis);
    timers.add(timer);
    return timer;
  }

  function cancel(timer: ReturnType<typeof setTimeout> | null): null {
    if (timer !== null) {
      clearTimeout(timer);
      timers.delete(timer);
    }
    return null;
  }

  function restartIdleTimer(): void {
    idle = false;
    idleTimer = cancel(idleTimer);
    idleTimer = later(() => {
      idleTimer = null;
      idle = true;
    }, AGENT_CURSOR_IDLE_MILLIS);
  }

  function showLabel(action: AgentCursorAction): void {
    labelAction = action;
    labelTimer = cancel(labelTimer);
    labelTimer = later(() => {
      labelTimer = null;
      labelAction = null;
    }, AGENT_CURSOR_ACTION_HOLD_MILLIS);
  }

  function playClick(delay: number): void {
    later(() => {
      const id = ++rippleSequence;
      ripples = [...ripples, id];
      pressing = true;
      later(() => { pressing = false; }, PRESS_MILLIS);
      later(() => { ripples = ripples.filter((ripple) => ripple !== id); }, RIPPLE_MILLIS);
    }, delay);
  }

  function hide(): void {
    if (!shown || leaving) return;
    leaving = true;
    labelAction = null;
    idle = false;
    labelTimer = cancel(labelTimer);
    idleTimer = cancel(idleTimer);
    leaveTimer = cancel(leaveTimer);
    leaveTimer = later(() => {
      leaveTimer = null;
      leaving = false;
      shown = false;
      point = null;
      ripples = [];
    }, LEAVE_MILLIS);
  }

  function apply(next: AgentCursorSignal): void {
    if (!next.visible || next.point === null) {
      hide();
      return;
    }
    leaveTimer = cancel(leaveTimer);
    const appearing = !shown || leaving;
    const previous = appearing ? null : point;
    glideMillis = agentCursorGlideMillis(previous, next.point);
    leaving = false;
    if (appearing) {
      shown = true;
      entering = true;
      later(() => { entering = false; }, 220);
    }
    const stationary = isStationaryMove(previous, next.point);
    point = next.point;
    restartIdleTimer();
    if (next.action === 'click') {
      showLabel('click');
      playClick(glideMillis);
    } else if (next.action === 'type' || next.action === 'scroll' || next.action === 'drag') {
      showLabel(next.action);
    } else if (!stationary) {
      // 真正移向新目标时清掉上一个动作的标签；点击收尾的原地 released 不清。
      labelTimer = cancel(labelTimer);
      labelAction = null;
    }
  }

  $effect(() => {
    const next = signal;
    if (!next || next.sequence === lastSequence) return;
    lastSequence = next.sequence;
    apply(next);
  });

  $effect(() => {
    if (!layer) return;
    if (active) layer.showPopover?.();
    else layer.hidePopover?.();
  });

  onDestroy(() => {
    for (const timer of timers) clearTimeout(timer);
    timers.clear();
  });
</script>

<div
  bind:this={layer}
  bind:clientWidth={layerWidth}
  bind:clientHeight={layerHeight}
  class="agent-cursor-layer"
  class:leaving
  style:position-anchor={anchorName}
  popover="manual"
  aria-hidden="true"
>
  <div class="agent-cursor-frame"></div>
  {#if point}
    <div
      class="agent-cursor"
      class:entering
      class:idle
      class:pressing
      style:transform={`translate3d(${point.x}px, ${point.y}px, 0)`}
      style:--agent-cursor-glide={`${glideMillis}ms`}
    >
      <span class="agent-cursor-halo"></span>
      {#each ripples as ripple (ripple)}
        <span class="agent-cursor-ripple"></span>
      {/each}
      <svg class="agent-cursor-arrow" width="24" height="24" viewBox="0 0 26 26" focusable="false">
        <path d="M5.05 3.55c-.77-.28-1.53.36-1.33 1.18l4.19 16.55c.28 1.1 1.74 1.34 2.28.32l3.7-6.95c.11-.21.28-.39.48-.51l6.35-3.61c1.16-.66.98-2.38-.3-2.79L5.05 3.55Z" />
      </svg>
      <span class="agent-cursor-tag" class:flip-x={placement.flipX} class:flip-y={placement.flipY}>
        <span class="agent-cursor-name">{i18n.t('browser.agent.cursor.name')}</span>
        {#if labelKey}
          <span class="agent-cursor-action">
            {i18n.t(labelKey)}
            {#if labelAction === 'type'}
              <span class="agent-cursor-dots"><span></span><span></span><span></span></span>
            {/if}
          </span>
        {/if}
      </span>
    </div>
  {/if}
</div>

<style>
  /* Popover 默认会居中并带边框与背景，这里改为贴合内容槽的透明层。 */
  .agent-cursor-layer {
    position: fixed;
    inset: auto;
    top: anchor(top);
    left: anchor(left);
    width: anchor-size(width);
    height: anchor-size(height);
    box-sizing: border-box;
    margin: 0;
    padding: 0;
    border: 0;
    overflow: hidden;
    background: transparent;
    color: inherit;
    pointer-events: none;
    opacity: 1;
    transition: opacity 180ms ease-out;
  }
  .agent-cursor-layer.leaving { opacity: 0; }

  /* 受控期间沿内容槽内侧的一圈描边，提示整页正由 Magi 操作。 */
  .agent-cursor-frame {
    position: absolute;
    inset: 0;
    box-shadow:
      inset 0 0 0 1.5px color-mix(in srgb, var(--primary) 70%, transparent),
      inset 0 0 22px color-mix(in srgb, var(--primary) 16%, transparent);
  }

  .agent-cursor {
    position: absolute;
    top: 0;
    left: 0;
    width: 0;
    height: 0;
    transition: transform var(--agent-cursor-glide, 0ms) cubic-bezier(0.22, 0.8, 0.26, 1);
    will-change: transform;
  }
  .agent-cursor.entering { animation: agent-cursor-enter 220ms cubic-bezier(0.2, 0.9, 0.3, 1.2); }

  /* 箭头尖端与落点对齐；描边与投影保证在任意网页底色上都清晰。 */
  .agent-cursor-arrow {
    position: absolute;
    left: -4px;
    top: -4px;
    overflow: visible;
    transform-origin: 4px 4px;
    transition: transform 140ms cubic-bezier(0.3, 0.7, 0.4, 1);
    filter: drop-shadow(0 1px 1.5px rgba(8, 10, 24, 0.45)) drop-shadow(0 3px 8px rgba(8, 10, 24, 0.22));
  }
  .agent-cursor-arrow path {
    fill: var(--primary);
    stroke: #fff;
    stroke-width: 1.8;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .agent-cursor.pressing .agent-cursor-arrow { transform: scale(0.84); }

  .agent-cursor-ripple {
    position: absolute;
    left: -14px;
    top: -14px;
    width: 28px;
    height: 28px;
    box-sizing: border-box;
    border: 2px solid var(--primary);
    border-radius: 50%;
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    animation: agent-cursor-ripple 560ms cubic-bezier(0.2, 0.7, 0.3, 1) forwards;
  }

  /* 等待模型下一步时的轻微呼吸，表示 Magi 仍在掌控而不是卡住。 */
  .agent-cursor-halo {
    position: absolute;
    left: -13px;
    top: -13px;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--primary) 22%, transparent);
    opacity: 0;
    transform: scale(0.6);
  }
  .agent-cursor.idle .agent-cursor-halo { animation: agent-cursor-breathe 2.4s ease-in-out infinite; }

  .agent-cursor-tag {
    position: absolute;
    left: 14px;
    top: 18px;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 180px;
    height: 20px;
    padding: 0 8px;
    border-radius: 9999px;
    background: var(--primary);
    box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.7), 0 4px 12px rgba(8, 10, 24, 0.24);
    color: var(--primary-foreground, #fff);
    font-family: var(--font-sans, inherit);
    font-size: 11px;
    font-weight: 600;
    line-height: 1;
    white-space: nowrap;
    transition: transform 160ms ease-out;
  }
  .agent-cursor-tag.flip-x { left: auto; right: 10px; }
  .agent-cursor-tag.flip-y { top: -30px; }
  .agent-cursor-name { letter-spacing: 0.01em; }
  .agent-cursor-action {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding-left: 5px;
    border-left: 1px solid color-mix(in srgb, var(--primary-foreground, #fff) 35%, transparent);
    font-weight: 500;
    opacity: 0.92;
    animation: agent-cursor-label-in 160ms ease-out;
  }
  .agent-cursor-dots { display: inline-flex; gap: 2px; }
  .agent-cursor-dots span {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: currentColor;
    animation: agent-cursor-dot 1s ease-in-out infinite;
  }
  .agent-cursor-dots span:nth-child(2) { animation-delay: 0.15s; }
  .agent-cursor-dots span:nth-child(3) { animation-delay: 0.3s; }

  @keyframes agent-cursor-enter {
    from { opacity: 0; scale: 0.6; }
    to { opacity: 1; scale: 1; }
  }
  @keyframes agent-cursor-ripple {
    from { opacity: 0.85; transform: scale(0.35); }
    to { opacity: 0; transform: scale(1.6); }
  }
  @keyframes agent-cursor-breathe {
    0%, 100% { opacity: 0; transform: scale(0.6); }
    50% { opacity: 1; transform: scale(1.15); }
  }
  @keyframes agent-cursor-label-in {
    from { opacity: 0; transform: translateX(-3px); }
    to { opacity: 0.92; transform: none; }
  }
  @keyframes agent-cursor-dot {
    0%, 100% { opacity: 0.35; transform: translateY(0); }
    50% { opacity: 1; transform: translateY(-2px); }
  }

  /* 减少动态效果：瞬移到落点，点击只做淡出提示，不呼吸、不跳动。 */
  @media (prefers-reduced-motion: reduce) {
    .agent-cursor,
    .agent-cursor-arrow,
    .agent-cursor-tag { transition: none; }
    .agent-cursor.entering,
    .agent-cursor.idle .agent-cursor-halo,
    .agent-cursor-action,
    .agent-cursor-dots span { animation: none; }
    .agent-cursor.pressing .agent-cursor-arrow { transform: none; }
    .agent-cursor-ripple { animation: agent-cursor-fade 400ms linear forwards; transform: none; }
    @keyframes agent-cursor-fade {
      from { opacity: 0.7; }
      to { opacity: 0; }
    }
  }
</style>
