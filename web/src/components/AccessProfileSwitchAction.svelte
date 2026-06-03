<script lang="ts">
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  function requestFullAccessProfile() {
    if (typeof window === 'undefined') return;
    window.dispatchEvent(new CustomEvent('magi:setAccessProfile', {
      detail: { profile: 'full_access' },
    }));
  }
</script>

<div class="permission-actions">
  <button type="button" class="permission-action-btn" onclick={requestFullAccessProfile}>
    <Icon name="shield" size={13} />
    {i18n.t('toolCall.permission.switchFullAccess')}
  </button>
</div>

<style>
  .permission-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-top: var(--space-3);
  }

  .permission-action-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 28px;
    padding: 0 var(--space-3);
    border: 1px solid rgba(234, 179, 8, 0.45);
    border-radius: var(--radius-sm);
    background: rgba(234, 179, 8, 0.12);
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: 500;
    cursor: pointer;
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  .permission-action-btn:hover {
    border-color: rgba(234, 179, 8, 0.65);
    background: rgba(234, 179, 8, 0.18);
  }
</style>
