<script lang="ts">
  import { getContext } from 'svelte';
  import { normalizeInlineFileReferenceTarget } from '../../lib/file-reference';
  import FileReferenceInline from './FileReferenceInline.svelte';

  interface Props {
    raw: string;
  }

  const { raw }: Props = $props();
  const insideLink = getContext<boolean>('markdown-link-context') === true;
  const codeText = $derived(raw.replace(/^`+|`+$/gu, ''));
  const fileTarget = $derived(normalizeInlineFileReferenceTarget(codeText));
</script>

{#if fileTarget && !insideLink}
  <FileReferenceInline label={codeText} target={fileTarget} variant="code" />
{:else}
  <code>{codeText}</code>
{/if}
