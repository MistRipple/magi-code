<script lang="ts">
  import { confirmDialogState, settleConfirm } from '../stores/confirm-dialog.svelte';
  import Modal from './Modal.svelte';

  const request = $derived(confirmDialogState.request);
</script>

{#if request}
  <Modal title={request.title} size="sm" onClose={() => settleConfirm('cancel')} closeOnBackdrop>
    <p class="confirm-message">{request.message}</p>

    {#snippet footer()}
      <div class="confirm-footer">
        <button type="button" class="confirm-btn" onclick={() => settleConfirm('cancel')}>
          {request.cancelLabel}
        </button>
        {#if request.secondaryLabel}
          <button type="button" class="confirm-btn confirm-btn--secondary" onclick={() => settleConfirm('secondary')}>
            {request.secondaryLabel}
          </button>
        {/if}
        <button
          type="button"
          class="confirm-btn"
          class:confirm-btn--primary={request.tone !== 'danger'}
          class:confirm-btn--danger={request.tone === 'danger'}
          onclick={() => settleConfirm('confirm')}
        >
          {request.confirmLabel}
        </button>
      </div>
    {/snippet}
  </Modal>
{/if}

<style>
  .confirm-message {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    line-height: 1.6;
    white-space: pre-line;
  }

  .confirm-footer {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .confirm-btn {
    height: 28px;
    padding: 0 14px;
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-sm);
    white-space: nowrap;
  }

  .confirm-btn:hover {
    background: color-mix(in srgb, var(--foreground) 7%, transparent);
    color: var(--foreground);
  }

  .confirm-btn--secondary {
    border: 1px solid var(--border);
    color: var(--foreground);
  }

  .confirm-btn--primary,
  .confirm-btn--primary:hover {
    background: var(--primary);
    color: var(--primary-foreground, #fff);
    font-weight: var(--font-semibold);
  }

  .confirm-btn--danger,
  .confirm-btn--danger:hover {
    background: var(--error);
    color: var(--primary-foreground, #fff);
    font-weight: var(--font-semibold);
  }
</style>
