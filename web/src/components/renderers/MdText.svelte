<script lang="ts">
  import { getContext } from 'svelte';
  import type { Snippet } from 'svelte';
  import { splitFileReferenceText } from '../../lib/file-reference';
  import FileReferenceInline from './FileReferenceInline.svelte';

  interface Props {
    text?: string;
    raw?: string;
    children?: Snippet;
  }

  const { text = '', raw = '', children }: Props = $props();
  const insideLink = getContext<boolean>('markdown-link-context') === true;
  const content = $derived(text || raw);
  const segments = $derived(splitFileReferenceText(content));
</script>

{#if insideLink}
  {@render children?.()}
{:else}
  {#each segments as segment}
    {#if segment.kind === 'file'}
      <FileReferenceInline label={segment.text} target={segment.target} />
    {:else}
      {segment.text}
    {/if}
  {/each}
{/if}
