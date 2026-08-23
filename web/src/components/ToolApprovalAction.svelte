<script lang="ts">
  import Icon from './Icon.svelte';
  import type { ToolApprovalPayload } from '../lib/tool-error-payload';
  import {
    isToolApprovalPending,
    resolvePendingToolApproval,
    toolApprovalState,
  } from '../stores/tool-approval-store.svelte';
  import type { ToolApprovalDecision } from '../shared/rust-backend-types';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    approval: ToolApprovalPayload;
  }

  let { approval }: Props = $props();
  let resolving = $state<ToolApprovalDecision | null>(null);
  let resolved = $state(false);
  let actionError = $state('');
  const approvalIsActive = $derived(
    !toolApprovalState.hydrated
      || toolApprovalState.sessionId !== approval.sessionId
      || isToolApprovalPending(approval.sessionId, approval.approvalId),
  );

  async function resolve(decision: ToolApprovalDecision): Promise<void> {
    if (resolving || resolved) return;
    resolving = decision;
    actionError = '';
    try {
      await resolvePendingToolApproval(approval.sessionId, approval.approvalId, decision);
      resolved = true;
    } catch (error) {
      actionError = error instanceof Error ? error.message : i18n.t('toolCall.approval.resolveFailed');
    } finally {
      resolving = null;
    }
  }
</script>

<div class="tool-approval" aria-live="polite">
  <div class="tool-approval-heading">
    <Icon name="shield" size={14} />
    <span>{i18n.t('toolCall.approval.title')}</span>
  </div>
  <p>{approval.reason || i18n.t('toolCall.approval.description')}</p>
  <div class="tool-approval-actions">
    <button
      type="button"
      class="approval-button approval-button--primary"
      disabled={Boolean(resolving) || resolved || !approvalIsActive}
      onclick={() => void resolve('allow_once')}
    >
      {resolving === 'allow_once' ? i18n.t('toolCall.approval.processing') : i18n.t('toolCall.approval.allowOnce')}
    </button>
    <button
      type="button"
      class="approval-button"
      disabled={Boolean(resolving) || resolved || !approvalIsActive}
      onclick={() => void resolve('allow_for_turn')}
    >
      {resolving === 'allow_for_turn' ? i18n.t('toolCall.approval.processing') : i18n.t('toolCall.approval.allowForTurn')}
    </button>
    <button
      type="button"
      class="approval-button"
      disabled={Boolean(resolving) || resolved || !approvalIsActive}
      onclick={() => void resolve('deny')}
    >
      {resolving === 'deny' ? i18n.t('toolCall.approval.processing') : i18n.t('toolCall.approval.deny')}
    </button>
  </div>
  {#if resolved}
    <div class="tool-approval-status">
      <Icon name="check" size={12} />
      {i18n.t('toolCall.approval.resolved')}
    </div>
  {:else if actionError}
    <div class="tool-approval-error">{actionError}</div>
  {:else if !approvalIsActive}
    <div class="tool-approval-status">
      <Icon name="check" size={12} />
      {i18n.t('toolCall.approval.notPending')}
    </div>
  {/if}
</div>

<style>
  .tool-approval {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-width: 0;
  }

  .tool-approval-heading,
  .tool-approval-status {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .tool-approval-heading :global(svg),
  .tool-approval-status :global(svg) {
    color: var(--foreground-muted);
  }

  .tool-approval p {
    margin: 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .tool-approval-actions {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .approval-button {
    min-height: 28px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .approval-button:hover:not(:disabled) {
    background: var(--surface-1);
    color: var(--foreground);
  }

  .approval-button--primary {
    border-color: var(--primary);
    background: var(--primary);
    color: var(--primary-foreground, #fff);
  }

  .approval-button--primary:hover:not(:disabled) {
    background: var(--primary-hover, var(--primary));
    color: var(--primary-foreground, #fff);
  }

  .approval-button:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .tool-approval-status,
  .tool-approval-error {
    font-size: var(--text-xs);
    font-weight: 400;
  }

  .tool-approval-status {
    color: var(--foreground-muted);
  }

  .tool-approval-error {
    color: var(--error);
  }
</style>
