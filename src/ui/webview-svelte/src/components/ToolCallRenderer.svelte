<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import ToolCall from './ToolCall.svelte';

  interface Props {
    block: ContentBlock;
  }

  let { block }: Props = $props();
</script>

<ToolCall
  name={block.toolCall?.name || 'Tool'}
  id={block.toolCall?.id}
  input={block.toolCall?.arguments}
  status={block.toolCall?.status}
  output={block.toolCall?.result}
  error={block.toolCall?.error}
  duration={block.toolCall?.endTime && block.toolCall?.startTime ? block.toolCall.endTime - block.toolCall.startTime : undefined}
/>
