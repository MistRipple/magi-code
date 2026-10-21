<!--
  Markdown 图片 renderer — 安全图片展示，限制最大宽度
  接收 @humanspeak/svelte-markdown 传入的 href + title + text props
-->
<script lang="ts">
  import { getContext } from 'svelte';
  import { agentUrl, buildFilePreviewQuery } from '../../web/agent-api';
  import { dispatchFilePreviewEvent } from '../../lib/file-reference';
  import { vscode } from '../../lib/vscode-bridge';
  import { desktopContextMenu, type DesktopContextMenuDescriptor } from '../../lib/desktop-context-menu-contract';
  import {
    MARKDOWN_IMAGE_CONTEXT,
    markdownImageScope,
    resolveMarkdownImageFilePath,
    type MarkdownImageContext,
  } from '../../lib/markdown-image';

  interface Props {
    href?: string;
    title?: string;
    text?: string;
  }
  const { href = '', title = undefined, text = '' }: Props = $props();
  const imageContext = getContext<MarkdownImageContext | undefined>(MARKDOWN_IMAGE_CONTEXT);
  const localFilePath = $derived(resolveMarkdownImageFilePath(href, imageContext?.baseFilePath));
  const fileScope = $derived(markdownImageScope(imageContext?.readFilePreviewScope));
  const externalImageUrl = $derived(/^https?:\/\//iu.test(href) ? href : '');
  const imageSource = $derived.by(() => {
    if (!localFilePath) return href;
    return agentUrl('/api/files/raw', buildFilePreviewQuery(localFilePath, fileScope));
  });

  function openImage(): void {
    if (localFilePath) {
      if (dispatchFilePreviewEvent({ filepath: localFilePath, ...fileScope })) return;
      vscode.postMessage({ type: 'openFile', filepath: localFilePath, ...fileScope });
      return;
    }
    if (externalImageUrl) {
      vscode.postMessage({ type: 'openLink', url: externalImageUrl });
    }
  }

  const contextDescriptor = $derived.by((): DesktopContextMenuDescriptor => localFilePath
    ? { kind: 'image', filePath: localFilePath, fileScope, open: openImage }
    : { kind: 'image', source: externalImageUrl || undefined, open: externalImageUrl ? openImage : undefined });
</script>

<button
  type="button"
  class="md-image-button"
  title={title}
  disabled={!localFilePath && !externalImageUrl}
  use:desktopContextMenu={contextDescriptor}
  onclick={openImage}
>
  <img
    src={imageSource}
    alt={text}
    {title}
    loading="lazy"
    class="md-image"
  />
</button>

<style>
  .md-image-button {
    display: inline-block;
    max-width: 100%;
    margin: 0;
    padding: 0;
    border: 0;
    background: transparent;
    text-align: left;
    cursor: pointer;
  }

  .md-image-button:disabled {
    cursor: default;
  }

  .md-image-button:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  .md-image {
    max-width: 100%;
    height: auto;
    border-radius: var(--radius-sm);
  }
</style>
