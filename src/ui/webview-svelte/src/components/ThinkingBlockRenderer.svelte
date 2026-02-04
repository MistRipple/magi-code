<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import ThinkingBlock from './ThinkingBlock.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
  }

  let { block, isStreaming = false }: Props = $props();

  // 🔧 修复：确保 thinking 内容在流式期间也能正确获取
  const thinkingContent = $derived(block.thinking?.content || block.content || '');

  // 🔧 修复：流式期间 isComplete 应该为 false，确保 ThinkingBlock 实时渲染
  // 优先使用 block.thinking?.isComplete，但如果是流式状态则强制为 false
  const isComplete = $derived(isStreaming ? false : (block.thinking?.isComplete ?? true));
</script>

<!-- 🔧 修复：流式期间默认展开 thinking 面板，让用户能看到实时输出 -->
<ThinkingBlock
  thinking={[{ content: thinkingContent }]}
  isStreaming={!isComplete}
  initialExpanded={isStreaming}
/>
