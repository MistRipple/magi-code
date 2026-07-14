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
  import { i18n } from '../stores/i18n.svelte';

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
  <div
    class="generated-image-progress"
    style:aspect-ratio={requestedAspectRatio}
    role="status"
    aria-live="polite"
  >
    <div class="generated-image-shimmer"></div>
    <div class="generated-image-progress-label">
      <span class="generated-image-progress-icon"><Icon name="sparkles" size={15} /></span>
      <span>{i18n.t('messageItem.generatedImagePending')}</span>
      <span class="generated-image-progress-dots" aria-hidden="true">
        <span></span><span></span><span></span>
      </span>
    </div>
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

  .generated-image-progress {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    width: min(100%, 520px);
    min-height: 180px;
    max-height: 420px;
    margin: var(--space-2) 0 var(--space-3);
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-subtle);
  }

  .generated-image-shimmer {
    position: absolute;
    inset: 0;
    background:
      linear-gradient(110deg, transparent 20%, color-mix(in srgb, var(--foreground) 8%, transparent) 42%, transparent 64%),
      radial-gradient(circle at 50% 45%, color-mix(in srgb, var(--primary) 10%, transparent), transparent 55%);
    background-size: 220% 100%, 100% 100%;
    animation: generated-image-shimmer 1.7s ease-in-out infinite;
  }

  .generated-image-progress-label {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border: 1px solid color-mix(in srgb, var(--border) 76%, transparent);
    border-radius: 999px;
    background: color-mix(in srgb, var(--surface-1) 88%, transparent);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    backdrop-filter: blur(8px);
  }

  .generated-image-progress-icon {
    display: inline-flex;
    color: var(--primary);
    animation: generated-image-pulse 1.4s ease-in-out infinite;
  }

  .generated-image-progress-dots {
    display: inline-flex;
    align-items: center;
    justify-content: flex-start;
    width: 1.35em;
    gap: 0.2em;
  }

  .generated-image-progress-dots span {
    width: 0.24em;
    height: 0.24em;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.28;
    animation: generated-image-dot 1.25s ease-in-out infinite;
  }

  .generated-image-progress-dots span:nth-child(2) { animation-delay: 0.16s; }
  .generated-image-progress-dots span:nth-child(3) { animation-delay: 0.32s; }

  @keyframes generated-image-shimmer {
    0% { background-position: 140% 0, 0 0; }
    100% { background-position: -120% 0, 0 0; }
  }

  @keyframes generated-image-pulse {
    0%, 100% { opacity: 0.45; }
    50% { opacity: 1; }
  }

  @keyframes generated-image-reduced-pulse {
    0%, 100% { opacity: 0.72; }
    50% { opacity: 1; }
  }

  @keyframes generated-image-dot {
    0%, 100% { opacity: 0.28; }
    50% { opacity: 1; }
  }

  @media (max-width: 640px) {
    .generated-image-progress {
      width: min(100%, 420px);
      min-height: 150px;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .generated-image-shimmer {
      animation: none;
      opacity: 0.65;
    }

    .generated-image-progress-icon {
      animation: none;
    }

    .generated-image-progress-label {
      animation: generated-image-reduced-pulse 1.8s ease-in-out infinite;
    }
  }
</style>
