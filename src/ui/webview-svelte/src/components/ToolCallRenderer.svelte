<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import ToolCall from './ToolCall.svelte';

  interface Props {
    block: ContentBlock;
  }

  let { block }: Props = $props();

  // 从工具参数中提取文件路径（file_edit/file_create/file_insert/file_remove）
  const filepath = $derived.by(() => {
    const args = block.toolCall?.arguments;
    if (!args || typeof args !== 'object') return undefined;
    const path = (args as Record<string, unknown>).path;
    return typeof path === 'string' ? path : undefined;
  });
</script>

<ToolCall
  name={block.toolCall?.name || 'Tool'}
  id={block.toolCall?.id}
  input={block.toolCall?.arguments}
  status={block.toolCall?.status}
  output={block.toolCall?.result}
  error={block.toolCall?.error}
  duration={block.toolCall?.endTime && block.toolCall?.startTime ? block.toolCall.endTime - block.toolCall.startTime : undefined}
  {filepath}
/>
