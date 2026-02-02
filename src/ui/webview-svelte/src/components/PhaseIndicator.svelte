<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';

  const appState = getState();

  // 阶段列表
  const phases = ['分析', '确认', '执行', '联调', '验证', '恢复', '汇总'];

  // 当前阶段 (1-7)
  const currentPhase = $derived(appState.currentPhase || 0);

  // 当前阶段描述
  const currentPhaseDesc = $derived(
    currentPhase > 0 && currentPhase <= phases.length
      ? `${phases[currentPhase - 1]}中...`
      : ''
  );
</script>

<div class="phase-indicator" class:hidden={currentPhase === 0}>
  <!-- 阶段步骤指示器 -->
  <div class="phase-steps">
    {#each phases as phase, index}
      {#if index > 0}
        <div class="phase-step-connector" class:active={currentPhase > index}></div>
      {/if}
      <div
        class="phase-step"
        class:active={currentPhase === index + 1}
        class:completed={currentPhase > index + 1}
        data-phase={index + 1}
        title={phase}
      >
        {#if currentPhase > index + 1}
          <Icon name="check" size={10} />
        {:else if currentPhase === index + 1}
          <Icon name="loader" size={10} class="spinning" />
        {:else}
          <span class="phase-number">{index + 1}</span>
        {/if}
      </div>
    {/each}
  </div>

  <!-- 当前阶段描述 -->
  {#if currentPhaseDesc}
    <div class="phase-desc">{currentPhaseDesc}</div>
  {/if}
</div>

<style>
  .phase-indicator {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    background: var(--background);
    border-bottom: 1px solid var(--border);
  }

  .phase-indicator.hidden {
    display: none;
  }

  /* 阶段步骤 */
  .phase-steps {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0;
    margin-top: var(--space-1);
  }

  .phase-step {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-radius: var(--radius-full);
    transition: all var(--transition-fast);
  }

  .phase-number {
    font-size: 10px;
  }

  .phase-step.active {
    color: var(--primary);
    background: var(--primary-muted);
    box-shadow: 0 0 0 2px var(--primary);
  }

  .phase-step.completed {
    color: white;
    background: var(--success);
  }

  .phase-step-connector {
    width: var(--space-4);
    height: 2px;
    background: var(--border);
    margin: 0 2px;
    transition: background var(--transition-fast);
  }

  .phase-step-connector.active {
    background: var(--success);
  }

  /* 阶段描述 */
  .phase-desc {
    text-align: center;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  /* 旋转动画 */
  :global(.phase-step .spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
</style>
