<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import { dispatchFilePreviewEvent } from '../lib/file-reference';
  import { agentUrl, buildFilePreviewQuery } from '../web/agent-api';
  import {
    imageGenerationAspectRatio,
    isImageGenerationTool,
    parseImageGenerationPreview,
  } from '../lib/image-generation-preview';
  import Icon from './Icon.svelte';
  import ToolCardMeta from './ToolCardMeta.svelte';
  import FileName from './FileName.svelte';
  import { resolveToolDisplayName } from '../lib/tool-display-name';
  import { i18n } from '../stores/i18n.svelte';
  import { desktopContextMenu } from '../lib/desktop-context-menu-contract';

  interface Props {
    block: ContentBlock;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, filePreviewScope = undefined }: Props = $props();

  const toolCall = $derived(block.toolCall);
  const toolName = $derived(toolCall?.name || '');
  const status = $derived(toolCall?.status || 'success');
  const requestedAspectRatio = $derived(imageGenerationAspectRatio(toolCall?.arguments));
  const preview = $derived.by(() => {
    if (!isImageGenerationTool(toolName)) return null;
    return parseImageGenerationPreview(toolName, toolCall?.result);
  });
  const imageSrc = $derived.by(() => {
    if (!preview) return '';
    return agentUrl('/api/files/raw', buildFilePreviewQuery(preview.path, filePreviewScope));
  });

  function openImageFile(): void {
    if (!preview?.path) return;
    dispatchFilePreviewEvent({
      ...filePreviewScope,
      filepath: preview.path,
      contentKind: 'binary',
      mime: preview.mime,
      size: preview.bytes,
    });
  }
</script>

{#snippet header()}
  <div class="generated-image-header">
    <span class="generated-image-icon" aria-hidden="true"><Icon name="sparkles" size={15} /></span>
    <span class="generated-image-title">{resolveToolDisplayName(toolName, i18n)}</span>
    {#if preview}<span class="generated-image-filename" title={preview.path}><FileName name={preview.path.split(/[\\/]/).pop() || preview.path} /></span>{/if}
    <ToolCardMeta {status} label={i18n.t(`terminalSession.status.${status}`)} duration={toolCall?.durationMs} />
  </div>
{/snippet}

{#if preview && imageSrc}
  <figure class="message-card generated-image-block">
    {@render header()}
    <button
      type="button"
      class="generated-image-button"
      onclick={openImageFile}
      title={i18n.t('messageItem.generatedImageOpen')}
      use:desktopContextMenu={{ kind:'image', filePath:preview.path, fileScope:filePreviewScope, open:openImageFile }}
    >
      <img src={imageSrc} alt={i18n.t('messageItem.generatedImageAlt')} loading="lazy" />
    </button>
    {#if preview.revisedPrompt}<figcaption title={preview.revisedPrompt}>{preview.revisedPrompt}</figcaption>{/if}
  </figure>
{:else if status === 'pending' || status === 'running'}
  <div class="message-card generated-image-block" role="status" aria-live="polite">
    {@render header()}
    <div class="generated-image-progress" style:aspect-ratio={requestedAspectRatio}>
      <span>{i18n.t('messageItem.generatedImagePending')}</span>
    </div>
  </div>
{/if}

<style>
  .generated-image-block { width:100%; max-width:100%; margin:var(--space-2) 0 var(--space-3); overflow:hidden; }
  .generated-image-header { display:flex; align-items:center; gap:8px; min-height:42px; padding:9px var(--tool-card-padding); }
  .generated-image-icon { display:flex; flex:none; color:var(--foreground-muted); }
  .generated-image-title { font-size:var(--text-sm); font-weight:var(--font-medium); white-space:nowrap; }
  .generated-image-filename { display:flex; min-width:0; color:var(--foreground-muted); font-size:var(--text-xs); }
  .generated-image-button { display:block; width:100%; padding:0; border:0; background:transparent; cursor:pointer; }
  .generated-image-button img { display:block; width:100%; max-height:560px; object-fit:contain; background:var(--code-bg); }
  .generated-image-button:focus-visible { outline:0; box-shadow:inset 0 0 0 2px var(--primary); }
  figcaption { margin:0; padding:9px var(--tool-card-padding) 12px; color:var(--foreground-muted); font-size:var(--text-xs); overflow-wrap:anywhere; text-wrap:pretty; }
  .generated-image-progress { display:flex; align-items:center; justify-content:center; width:100%; min-height:150px; max-height:320px; background:var(--code-bg); color:var(--foreground-muted); font-size:var(--text-sm); }
  .generated-image-progress > span { animation:generated-image-pulse 1.5s ease-in-out infinite; }
  @keyframes generated-image-pulse { 50% { opacity:.4; } }
</style>
