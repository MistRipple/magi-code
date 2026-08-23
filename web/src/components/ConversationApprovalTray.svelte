<script lang="ts">
  import type { PendingToolApprovalDto } from '../shared/rust-backend-types';
  import ToolApprovalAction from './ToolApprovalAction.svelte';

  interface Props {
    approvals: PendingToolApprovalDto[];
  }

  let { approvals }: Props = $props();
</script>

{#if approvals.length > 0}
  <section class="conversation-approval-tray" aria-live="polite">
    {#each approvals as approval (approval.approvalId)}
      <div class="conversation-approval-item" data-approval-id={approval.approvalId}>
        <ToolApprovalAction approval={approval} />
      </div>
    {/each}
  </section>
{/if}

<style>
  .conversation-approval-tray {
    display: flex;
    flex-direction: column;
    flex: 0 0 auto;
    gap: var(--space-2);
    width: min(100%, var(--conversation-content-width, 100%));
    padding: var(--space-2) var(--space-4) var(--space-3);
  }

  .conversation-approval-item {
    min-width: 0;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
  }
</style>
