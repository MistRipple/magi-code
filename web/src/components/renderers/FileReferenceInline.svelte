<script lang="ts">
  import { getContext } from 'svelte';
  import {
    dispatchFilePreviewEvent,
    FILE_PREVIEW_SCOPE_CONTEXT,
    type FilePreviewScopeReader,
  } from '../../lib/file-reference';
  import { vscode } from '../../lib/vscode-bridge';

  interface Props {
    label: string;
    target: string;
    variant?: 'text' | 'code';
  }

  const { label, target, variant = 'text' }: Props = $props();
  const readFilePreviewScope = getContext<FilePreviewScopeReader | undefined>(FILE_PREVIEW_SCOPE_CONTEXT);

  function currentFilePreviewScope() {
    return readFilePreviewScope?.() ?? {};
  }

  function handleClick(event: MouseEvent) {
    event.preventDefault();
    event.stopPropagation();
    const scope = currentFilePreviewScope();
    if (dispatchFilePreviewEvent({ filepath: target, ...scope })) {
      return;
    }
    vscode.postMessage({ type: 'openFile', filepath: target, ...scope });
  }
</script>

<a
  href={target}
  title={target}
  class="md-file-ref md-file-ref--{variant}"
  onclick={handleClick}
>
  {#if variant === 'code'}
    <code>{label}</code>
  {:else}
    {label}
  {/if}
</a>

<style>
  .md-file-ref {
    color: var(--primary);
    cursor: pointer;
    overflow-wrap: anywhere;
    text-decoration: none;
  }

  .md-file-ref:hover {
    text-decoration: underline;
  }

  .md-file-ref--code {
    text-decoration: none;
  }

  .md-file-ref--code:hover code {
    text-decoration: underline;
  }

  .md-file-ref--code code {
    color: inherit;
  }
</style>
