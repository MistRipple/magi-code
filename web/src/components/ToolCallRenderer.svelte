<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import {
    isImageGenerationTool,
    parseImageGenerationPreview,
  } from '../lib/image-generation-preview';
  import { TERMINAL_TOOLS, normalizeTerminalToolName } from '../lib/terminal-utils';
  import GeneratedImageBlock from './GeneratedImageBlock.svelte';
  import ToolCall from './ToolCall.svelte';
  import TerminalSessionCard from './TerminalSessionCard.svelte';

  interface Props {
    block: ContentBlock;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, filePreviewScope = undefined }: Props = $props();

  const toolName = $derived(block.toolCall?.name || 'Tool');
  const toolStatus = $derived(block.toolCall?.status);
  const normalizedToolName = $derived(normalizeTerminalToolName(toolName));
  const isTerminalSessionTool = $derived(TERMINAL_TOOLS.has(normalizedToolName));
  // 工具名可能带有 bridge/MCP 命名空间，必须复用统一身份解析，避免真实结果退回工具卡片。
  const isGeneratedImageTool = $derived(isImageGenerationTool(toolName));
  const generatedImageResult = $derived(block.toolCall?.result);
  const hasGeneratedImagePreview = $derived(
    isGeneratedImageTool
      && toolStatus !== 'error'
      && parseImageGenerationPreview(toolName, generatedImageResult) !== null,
  );
</script>

{#if isGeneratedImageTool && (hasGeneratedImagePreview || toolStatus === 'pending' || toolStatus === 'running')}
  <GeneratedImageBlock
    {block}
    {filePreviewScope}
  />
{:else if isTerminalSessionTool}
  <TerminalSessionCard
    toolCall={block.toolCall}
    status={toolStatus}
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
    {filePreviewScope}
    duration={typeof block.toolCall?.durationMs === 'number'
      ? block.toolCall.durationMs
      : (block.toolCall?.endTime && block.toolCall?.startTime ? block.toolCall.endTime - block.toolCall.startTime : undefined)}
  />
{/if}
