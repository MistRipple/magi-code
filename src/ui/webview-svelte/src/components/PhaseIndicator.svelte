<script lang="ts">
  import { getState } from '../stores/messages.svelte';

  const appState = getState();

  // 阶段列表
  const phases = ['分析', '确认', '执行', '联调', '验证', '恢复', '汇总'];

  // 当前阶段 (1-7)
  const currentPhase = $derived(appState.currentPhase || 0);
</script>

<div class="phase-indicator" class:hidden={currentPhase === 0}>
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
      >
        {phase}
      </div>
    {/each}
  </div>
</div>

<style>
  .phase-indicator {
    flex-shrink: 0;
    padding: var(--space-2) var(--space-4);
    background: var(--background);
    border-bottom: 1px solid var(--border);
  }

  .phase-indicator.hidden {
    display: none;
  }

  .phase-steps {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0;
  }

  .phase-step {
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }

  .phase-step.active {
    color: var(--primary);
    background: var(--primary-muted);
  }

  .phase-step.completed {
    color: var(--success);
  }

  .phase-step-connector {
    width: var(--space-5);
    height: 2px;
    background: var(--border);
    margin: 0 var(--space-1);
    transition: background var(--transition-fast);
  }

  .phase-step-connector.active {
    background: var(--success);
  }
</style>

