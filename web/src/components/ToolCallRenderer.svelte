<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import {
    isImageGenerationTool,
    parseImageGenerationPreview,
  } from '../lib/image-generation-preview';
  import { TERMINAL_TOOLS, normalizeTerminalToolName } from '../lib/terminal-utils';
  import { parseToolApprovalPayload } from '../lib/tool-error-payload';
  import GeneratedImageBlock from './GeneratedImageBlock.svelte';
  import ToolCall from './ToolCall.svelte';
  import TerminalSessionCard from './TerminalSessionCard.svelte';
  import type { ConversationPresentationRole } from '../lib/conversation-presentation';
  import { getAgentMessageItem } from '../web/agent-api';
  import { valueToDisplayText } from '../stores/turn-projection';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    block: ContentBlock;
    filePreviewScope?: FilePreviewScope;
    presentationRole?: ConversationPresentationRole;
  }

  let { block, filePreviewScope = undefined, presentationRole = 'process' }: Props = $props();

  // 历史分页会把超长工具输出截成开头一段；用户点击后按需取回完整内容，只在本卡片内生效，
  // 不改写会话事实。
  let fullResult = $state<string | undefined>(undefined);
  let loadingFull = $state(false);
  let loadFullFailed = $state(false);
  const truncatedRef = $derived(fullResult === undefined ? block.toolCall?.truncatedResult : undefined);
  const effectiveToolCall = $derived(
    block.toolCall && fullResult !== undefined
      ? { ...block.toolCall, result: fullResult }
      : block.toolCall,
  );

  async function loadFullResult() {
    const ref = block.toolCall?.truncatedResult;
    if (!ref || loadingFull) {
      return;
    }
    loadingFull = true;
    loadFullFailed = false;
    try {
      const item = await getAgentMessageItem(ref);
      const text = valueToDisplayText(item.tool?.result);
      if (text === undefined) {
        loadFullFailed = true;
      } else {
        fullResult = text;
      }
    } catch {
      loadFullFailed = true;
    } finally {
      loadingFull = false;
    }
  }

  const toolName = $derived(block.toolCall?.name || 'Tool');
  const toolStatus = $derived(block.toolCall?.status);
  const normalizedToolName = $derived(normalizeTerminalToolName(toolName));
  const toolApproval = $derived(
    parseToolApprovalPayload(effectiveToolCall?.result)
      || parseToolApprovalPayload(block.toolCall?.error)
      || parseToolApprovalPayload(block.toolCall?.standardized?.message),
  );
  // 所有工具的授权交互统一交给 ToolCall；终端专用卡只处理真实终端输出。
  const isTerminalSessionTool = $derived(
    TERMINAL_TOOLS.has(normalizedToolName) && !toolApproval,
  );
  // 工具名可能带有 bridge/MCP 命名空间，必须复用统一身份解析，避免真实结果退回工具卡片。
  const isGeneratedImageTool = $derived(isImageGenerationTool(toolName));
  const generatedImageResult = $derived(effectiveToolCall?.result);
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
    toolCall={effectiveToolCall}
    status={toolStatus}
  />
{:else}
  <ToolCall
    name={toolName}
    id={block.toolCall?.id}
    input={block.toolCall?.arguments}
    status={toolStatus}
    output={effectiveToolCall?.result}
    error={block.toolCall?.error}
    standardized={block.toolCall?.standardized}
    {filePreviewScope}
    {presentationRole}
    duration={typeof block.toolCall?.durationMs === 'number'
      ? block.toolCall.durationMs
      : (block.toolCall?.endTime && block.toolCall?.startTime ? block.toolCall.endTime - block.toolCall.startTime : undefined)}
  />
{/if}

{#if truncatedRef}
  <div class="tool-output-truncated" data-testid="tool-output-truncated">
    <span>{i18n.t('toolCall.truncatedOutput', { omitted: truncatedRef.omittedChars })}</span>
    <button type="button" class="tool-output-load" disabled={loadingFull} onclick={loadFullResult}>
      {i18n.t('toolCall.loadFullOutput')}
    </button>
    {#if loadFullFailed}
      <span class="tool-output-error">{i18n.t('toolCall.loadFullOutputFailed')}</span>
    {/if}
  </div>
{/if}

<style>
  .tool-output-truncated {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-3);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .tool-output-load {
    color: var(--primary);
    background: none;
    border: 0;
    padding: 0;
    cursor: pointer;
    font: inherit;
  }

  .tool-output-load:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .tool-output-error {
    color: var(--error);
  }
</style>
