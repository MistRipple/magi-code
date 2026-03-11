<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import { TERMINAL_TOOLS, normalizeTerminalToolName } from '../lib/terminal-utils';
  import ToolCall from './ToolCall.svelte';
  import TerminalSessionCard from './TerminalSessionCard.svelte';

  interface Props {
    block: ContentBlock;
  }

  let { block }: Props = $props();

  const toolName = $derived(block.toolCall?.name || 'Tool');
  const toolStatus = $derived(block.toolCall?.status);
  const normalizedToolName = $derived(normalizeTerminalToolName(toolName));
  const isTerminalSessionTool = $derived(TERMINAL_TOOLS.has(normalizedToolName));
</script>

{#if isTerminalSessionTool}
  <TerminalSessionCard
    toolCall={block.toolCall}
    status={toolStatus}
    initialExpanded={Boolean(toolStatus === 'running' || toolStatus === 'pending' || toolStatus === 'error')}
  />
{:else}
  <ToolCall
    name={toolName}
    id={block.toolCall?.id}
    input={block.toolCall?.arguments}
    status={toolStatus}
    output={block.toolCall?.result}
    error={block.toolCall?.error}
    standardized={block.toolCall?.standardized}
    duration={block.toolCall?.endTime && block.toolCall?.startTime ? block.toolCall.endTime - block.toolCall.startTime : undefined}
    initialExpanded={toolStatus === 'error'}
  />
{/if}
