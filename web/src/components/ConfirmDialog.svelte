<script lang="ts">
  import { confirmDialogState, settleConfirm } from '../stores/confirm-dialog.svelte';
  import Modal from './Modal.svelte';
  import Popconfirm from './Popconfirm.svelte';

  const request = $derived(confirmDialogState.request);
</script>

{#if request?.anchor && !request.secondaryLabel}
  <Popconfirm anchor={request.anchor} title={request.title} message={request.message} confirmLabel={request.confirmLabel} cancelLabel={request.cancelLabel} onConfirm={() => settleConfirm('confirm')} onCancel={() => settleConfirm('cancel')} />
{:else if request}
  <Modal title={request.title} size="sm" onClose={() => settleConfirm('cancel')} closeOnBackdrop>
    <p class="confirm-message">{request.message}</p>

    {#snippet footer()}
      <div class="confirm-footer">
        <button type="button" class="btn btn--secondary btn--sm" onclick={() => settleConfirm('cancel')}>
          {request.cancelLabel}
        </button>
        {#if request.secondaryLabel}
          <button type="button" class="btn btn--secondary btn--sm" onclick={() => settleConfirm('secondary')}>
            {request.secondaryLabel}
          </button>
        {/if}
        <button
          type="button"
          class="btn btn--sm"
          class:btn--primary={request.tone !== 'danger'}
          class:btn--error={request.tone === 'danger'}
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
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
