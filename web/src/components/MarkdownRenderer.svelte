<script lang="ts">
  import { setContext } from 'svelte';
  import SvelteMarkdown from '@humanspeak/svelte-markdown';
  import MdCodeBlock from './renderers/MdCodeBlock.svelte';
  import MdLink from './renderers/MdLink.svelte';
  import MdImage from './renderers/MdImage.svelte';

  interface Props {
    source: string;
    isStreaming?: boolean;
  }

  let { source, isStreaming = false }: Props = $props();

  setContext('markdown-streaming', {
    get isStreaming() {
      return isStreaming;
    },
  });

  const renderers = {
    code: MdCodeBlock,
    link: MdLink,
    image: MdImage,
  };

  const options = {
    breaks: true,
    gfm: true,
  };
</script>

<SvelteMarkdown
  {source}
  {renderers}
  {options}
/>
