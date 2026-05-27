<script lang="ts">
  import { dispatchFilePreviewEvent } from '../../lib/file-reference';
  import { vscode } from '../../lib/vscode-bridge';

  interface Props {
    label: string;
    target: string;
    variant?: 'text' | 'code';
  }

  const { label, target, variant = 'text' }: Props = $props();

  function handleClick(event: MouseEvent) {
    event.preventDefault();
    event.stopPropagation();
    if (dispatchFilePreviewEvent({ filepath: target })) {
      return;
    }
    vscode.postMessage({ type: 'openFile', filepath: target });
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
