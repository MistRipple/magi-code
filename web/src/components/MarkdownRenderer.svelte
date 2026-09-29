<script lang="ts">
  import { setContext } from 'svelte';
  import SvelteMarkdown from '@humanspeak/svelte-markdown';
  import MdCodeBlock from './renderers/MdCodeBlock.svelte';
  import MdCodeSpan from './renderers/MdCodeSpan.svelte';
  import MdLink from './renderers/MdLink.svelte';
  import MdImage from './renderers/MdImage.svelte';
  import MdText from './renderers/MdText.svelte';
  import { sanitizeMarkdownUrl } from '../lib/markdown-url';

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

  // 文件引用识别挂在 rawtext（叶子文本）上，而不是 text：
  // 紧凑列表项的 text token 带有嵌套的行内 token（加粗、行内代码、链接），
  // text 必须交给默认实现去渲染这些子节点，否则列表里的 **加粗** 和 `代码` 会显示成原始符号。
  const renderers = {
    code: MdCodeBlock,
    codespan: MdCodeSpan,
    link: MdLink,
    image: MdImage,
    rawtext: MdText,
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
  sanitizeUrl={sanitizeMarkdownUrl}
/>
