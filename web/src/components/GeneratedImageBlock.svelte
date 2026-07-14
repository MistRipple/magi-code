<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import { dispatchFilePreviewEvent } from '../lib/file-reference';
  import { agentUrl, buildFilePreviewQuery } from '../web/agent-api';
  import {
    isImageGenerationTool,
    parseImageGenerationPreview,
  } from '../lib/image-generation-preview';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    block: ContentBlock;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, filePreviewScope = undefined }: Props = $props();

  const toolCall = $derived(block.toolCall);
  const toolName = $derived(toolCall?.name || '');
  const status = $derived(toolCall?.status || 'success');
  const preview = $derived.by(() => {
    if (!isImageGenerationTool(toolName)) return null;
    return parseImageGenerationPreview(toolName, toolCall?.result);
  });
  const imageSrc = $derived.by(() => {
    if (!preview) return '';
    return agentUrl('/api/files/raw', buildFilePreviewQuery(preview.path, {
      workspaceId: filePreviewScope?.workspaceId,
      workspacePath: filePreviewScope?.workspacePath,
      sessionId: '',
    }));
  });

  function openImageFile(): void {
    if (!preview?.path) return;
    dispatchFilePreviewEvent({
      filepath: preview.path,
      workspaceId: filePreviewScope?.workspaceId,
      workspacePath: filePreviewScope?.workspacePath,
      sessionId: filePreviewScope?.sessionId,
      contentKind: 'binary',
      mime: preview.mime,
      size: preview.bytes,
    });
  }
</script>

{#if preview && imageSrc}
  <figure class="generated-image-block">
    <button
      type="button"
      class="generated-image-button"
      onclick={openImageFile}
      title={i18n.t('messageItem.generatedImageOpen')}
    >
      <img
        src={imageSrc}
        alt={i18n.t('messageItem.generatedImageAlt')}
        loading="lazy"
      />
    </button>
    {#if preview.revisedPrompt}
      <figcaption title={preview.revisedPrompt}>{preview.revisedPrompt}</figcaption>
    {/if}
  </figure>
{:else if status === 'pending' || status === 'running'}
  <div class="generated-image-pending" role="status" aria-live="polite">
    <Icon name="sparkles" size={14} />
    <span>{i18n.t('messageItem.generatedImagePending')}</span>
  </div>
{/if}

<style>
  .generated-image-block {
    width: fit-content;
    max-width: min(100%, 680px);
    margin: var(--space-2) 0 var(--space-3);
  }

  .generated-image-button {
    display: block;
    max-width: 100%;
    padding: 0;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    cursor: pointer;
    overflow: hidden;
  }

  .generated-image-button img {
    display: block;
    max-width: 100%;
    max-height: 560px;
    object-fit: contain;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-subtle);
  }

  .generated-image-button:hover img {
    border-color: var(--foreground-muted);
  }

  .generated-image-button:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 3px;
  }

  figcaption {
    max-width: 100%;
    margin-top: var(--space-1);
    overflow: hidden;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .generated-image-pending {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    margin: var(--space-2) 0;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .generated-image-pending :global(svg) {
    color: var(--primary);
    animation: generated-image-pulse 1.4s ease-in-out infinite;
  }

  @keyframes generated-image-pulse {
    0%, 100% { opacity: 0.45; }
    50% { opacity: 1; }
  }
</style>
