<script lang="ts">
  /**
   * GPT Web 对话方式选择：临时对话 / 已保存对话（新建或选择已有）。
   *
   * 只在会话还没有任何消息时出现；方式与引擎一样，在首条消息时固定。已保存对话的历史由
   * ChatGPT 侧拥有，这里只负责列出并把选中的那条交给父组件绑定。
   */
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { listWebSavedConversations, type WebSavedConversation } from '../web/agent-api';

  type WebMode = 'temporary' | 'saved';

  interface Props {
    mode: WebMode;
    disabled?: boolean;
    onModeChange: (mode: WebMode) => Promise<void> | void;
    onBindSaved: (conversationId: string) => Promise<void> | void;
  }

  let { mode, disabled = false, onModeChange, onBindSaved }: Props = $props();

  let listOpen = $state(false);
  let loading = $state(false);
  let error = $state('');
  let conversations = $state<WebSavedConversation[]>([]);
  let binding = $state('');

  async function toggleList(): Promise<void> {
    if (listOpen) {
      listOpen = false;
      return;
    }
    listOpen = true;
    loading = true;
    error = '';
    try {
      conversations = await listWebSavedConversations();
    } catch (cause) {
      conversations = [];
      error = cause instanceof Error && cause.message.trim()
        ? cause.message.trim()
        : i18n.t('webModel.saved.loadFailed');
    } finally {
      loading = false;
    }
  }

  async function choose(conversationId: string): Promise<void> {
    if (binding) return;
    binding = conversationId;
    error = '';
    try {
      await onBindSaved(conversationId);
      listOpen = false;
    } catch (cause) {
      error = cause instanceof Error && cause.message.trim()
        ? cause.message.trim()
        : i18n.t('webModel.saved.bindFailed');
    } finally {
      binding = '';
    }
  }
</script>

<div class="wm-mode" data-web-model-mode={mode}>
  <div class="wm-mode-header">{i18n.t('webModel.mode.header')}</div>
  <div class="wm-mode-strip" role="group">
    <button
      type="button"
      class="wm-mode-btn"
      class:selected={mode === 'temporary'}
      data-web-model-mode-option="temporary"
      {disabled}
      onclick={() => void onModeChange('temporary')}
    >{i18n.t('webModel.mode.temporary')}</button>
    <button
      type="button"
      class="wm-mode-btn"
      class:selected={mode === 'saved'}
      data-web-model-mode-option="saved"
      {disabled}
      onclick={() => void onModeChange('saved')}
    >{i18n.t('webModel.mode.saved')}</button>
  </div>
  <p class="wm-mode-hint">
    {mode === 'temporary' ? i18n.t('webModel.mode.temporaryHint') : i18n.t('webModel.mode.savedHint')}
  </p>
  <button
    type="button"
    class="wm-history-btn"
    data-web-model-history="1"
    {disabled}
    onclick={() => void toggleList()}
  >
    {i18n.t('webModel.saved.pick')}
    <Icon name="chevron-down" size={10} />
  </button>
  {#if listOpen}
    <div class="wm-history" role="listbox">
      {#if loading}
        <p class="wm-note">{i18n.t('webModel.saved.loading')}</p>
      {:else if error}
        <p class="wm-note wm-note--error">{error}</p>
      {:else if conversations.length === 0}
        <p class="wm-note">{i18n.t('webModel.saved.empty')}</p>
      {:else}
        {#each conversations as item (item.conversationId)}
          <button
            type="button"
            class="wm-history-item"
            data-web-model-history-item={item.conversationId}
            disabled={disabled || binding !== ''}
            onclick={() => void choose(item.conversationId)}
          >
            <span class="wm-history-title">{item.title || item.conversationId}</span>
            {#if binding === item.conversationId}
              <Icon name="loader" size={12} class="spinning" />
            {/if}
          </button>
        {/each}
      {/if}
    </div>
  {:else if error}
    <p class="wm-note wm-note--error">{error}</p>
  {/if}
</div>

<style>
  .wm-mode {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px 8px 8px;
  }

  .wm-mode-header {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .wm-mode-strip {
    display: flex;
    gap: 4px;
  }

  .wm-mode-btn,
  .wm-history-btn,
  .wm-history-item {
    padding: 3px 10px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  .wm-mode-btn.selected {
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    border-color: transparent;
  }

  .wm-mode-btn:disabled,
  .wm-history-btn:disabled,
  .wm-history-item:disabled {
    cursor: default;
    opacity: 0.5;
  }

  .wm-mode-hint,
  .wm-note {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .wm-note--error {
    color: var(--danger, #c53f4f);
  }

  .wm-history-btn {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .wm-history {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 180px;
    overflow-y: auto;
  }

  .wm-history-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    text-align: left;
    border-color: transparent;
  }

  .wm-history-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
