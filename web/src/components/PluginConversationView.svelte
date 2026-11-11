<script lang="ts">
  import PluginFrame from './tabs/PluginFrame.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { PluginConversationViewContext } from '../shared/app-server-protocol.generated';
  let { pluginId, entry, label, placement, context }: {
    pluginId: string; entry: string; label: string;
    placement: 'composerLeft' | 'chatAssistant'; context: PluginConversationViewContext;
  } = $props();
  const viewId = $props.id();
  let expanded = $state(false);
  let contentVisible = $state(true);
</script>

<!-- 每个可见位置拥有隔离文档；业务及注册协议仍共用唯一 PluginFrame。 -->
<div class="plugin-conversation-view" class:message-view={placement === 'chatAssistant'}
  class:composer-view={placement === 'composerLeft'} class:empty-composer={placement === 'composerLeft' && !contentVisible}
  inert={placement === 'composerLeft' && !contentVisible} data-plugin-placement={placement}>
  {#if placement === 'chatAssistant'}
    <button type="button" class="btn btn--secondary btn--sm" aria-expanded={expanded} aria-controls={viewId}
      onclick={() => { expanded = !expanded; }}>
      <Icon name={expanded ? 'chevron-down' : 'chevron-right'} size={14} />
      {i18n.t('plugins.conversationView', { name: label })}
    </button>
  {/if}
  <div id={viewId} hidden={placement === 'chatAssistant' && !expanded}>
    {#if placement !== 'chatAssistant' || expanded}
      <PluginFrame {pluginId} {entry} {label} {placement} conversation={context} frameTestId={`plugin-${placement}`}
        onContentVisibility={(visible) => { contentVisible = visible; }} />
    {/if}
  </div>
</div>

<style>
  .plugin-conversation-view { min-inline-size: 0; inline-size: 100%; }
  .composer-view { inline-size: 96px; flex: 0 1 96px; }
  /* 退出工具栏布局和 gap，但保留可测量的隔离视口，异步内容出现时可自行恢复。 */
  .empty-composer { position: absolute; visibility: hidden; pointer-events: none; }
  .message-view { display: grid; justify-items: start; gap: var(--space-2); margin-block-start: var(--space-3); }
  .message-view > div { min-inline-size: 0; inline-size: 100%; }
</style>
