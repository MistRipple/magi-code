<script lang="ts">
  import type { Message } from '../types/message';
  import type { IconName } from '../lib/icons';
  import MarkdownContent from './MarkdownContent.svelte';
  import StreamingIndicator from './StreamingIndicator.svelte';
  import WorkerBadge from './WorkerBadge.svelte';
  import ThinkingBlock from './ThinkingBlock.svelte';
  import ToolCall from './ToolCall.svelte';
  import CodeBlock from './CodeBlock.svelte';
  import Icon from './Icon.svelte';

  // Props
  interface Props {
    message: Message;
  }
  let { message }: Props = $props();

  // 派生状态
  const isUser = $derived(message.role === 'user');
  const isNotice = $derived(message.type === 'system-notice' || message.role === 'system');
  const isStreaming = $derived(message.isStreaming);

  // 格式化时间戳
  function formatTime(timestamp: number): string {
    const date = new Date(timestamp);
    return date.toLocaleTimeString('zh-CN', {
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  // 获取 worker 信息（如果有）
  const worker = $derived(message.metadata?.worker || null);

  // 通知类型和对应的图标/颜色（使用 Message 类型中的 noticeType）
  const noticeType = $derived(message.noticeType || 'info');
  const noticeIcons: Record<string, IconName> = {
    success: 'check-circle',
    error: 'x-circle',
    warning: 'alert-triangle',
    info: 'info'
  };
  const noticeColors: Record<string, string> = {
    success: '#22c55e',
    error: '#ef4444',
    warning: '#f59e0b',
    info: '#3b82f6'
  };

  // 复制内容
  async function handleCopy() {
    if (!message.content) return;
    try {
      await navigator.clipboard.writeText(message.content);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }
</script>

<!-- 系统通知消息：居中显示 -->
{#if isNotice}
  <div class="system-notice {noticeType}">
    <span class="notice-icon" style="color: {noticeColors[noticeType] || noticeColors.info}">
      <Icon name={noticeIcons[noticeType] || 'info'} size={14} />
    </span>
    <span class="notice-text">{message.content}</span>
    <span class="notice-time">{formatTime(message.timestamp)}</span>
  </div>
<!-- 用户消息：简洁显示 -->
{:else if isUser}
  <div class="message-item user" data-message-id={message.id}>
    <div class="user-content">{message.content}</div>
    <div class="user-time">{formatTime(message.timestamp)}</div>
  </div>
<!-- 助手消息：完整显示 -->
{:else}
  <div
    class="message-item assistant"
    class:streaming={isStreaming}
    data-message-id={message.id}
  >
    <div class="message-header">
      <div class="message-source">
        <!-- 只显示 WorkerBadge，不再重复显示 source-name -->
        <WorkerBadge worker={worker || message.source} size="sm" />
      </div>
      <div class="message-meta">
        <span class="message-time">{formatTime(message.timestamp)}</span>
        {#if !isStreaming}
          <button class="copy-btn" onclick={handleCopy} title="复制内容">
            <Icon name="copy" size={12} />
          </button>
        {/if}
      </div>
    </div>

    <div class="message-content">
      {#if message.blocks && message.blocks.length > 0}
        {#each message.blocks as block, i (i)}
          {#if block.type === 'thinking'}
            <ThinkingBlock
              thinking={[{ content: block.thinking?.content || block.content || '' }]}
              isStreaming={isStreaming && !block.thinking?.isComplete}
              initialExpanded={false}
            />
          {:else if block.type === 'tool_call'}
            <ToolCall
              name={block.toolCall?.name || 'Tool'}
              id={block.toolCall?.id}
              input={block.toolCall?.arguments}
              status={block.toolCall?.status}
              output={block.toolCall?.result}
              error={block.toolCall?.error}
              duration={block.toolCall?.endTime && block.toolCall?.startTime ? block.toolCall.endTime - block.toolCall.startTime : undefined}
            />
          {:else if block.type === 'code'}
            <CodeBlock
              code={block.content || ''}
              language={block.language || ''}
              showLineNumbers={true}
            />
          {:else}
            <MarkdownContent content={block.content || ''} {isStreaming} />
          {/if}
        {/each}
      {:else if message.content}
        <MarkdownContent content={message.content} {isStreaming} />
      {/if}

      {#if isStreaming && (!message.blocks || message.blocks.length === 0)}
        <StreamingIndicator />
      {/if}
    </div>
  </div>
{/if}

<style>
  /* ===== 系统通知样式（与HTML版本一致，简洁无背景） ===== */
  .system-notice {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 4px 12px;
    margin: 2px auto;
    font-size: var(--text-sm);
    width: fit-content;
    max-width: 90%;
  }
  .system-notice.info { color: var(--info); }
  .system-notice.success { color: var(--success); }
  .system-notice.warning { color: var(--warning); }
  .system-notice.error { color: var(--error); }
  .notice-icon {
    display: flex;
    flex-shrink: 0;
    width: 14px;
    height: 14px;
  }
  .notice-text {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .notice-time {
    font-size: 10px;
    opacity: 0.6;
    margin-left: 4px;
  }

  /* ===== 用户消息样式（简洁） ===== */
  .message-item.user {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    padding: var(--space-3) var(--space-4);
    margin-left: auto;
    max-width: 85%;
  }
  .user-content {
    background: var(--primary);
    color: white;
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-lg) var(--radius-lg) var(--radius-sm) var(--radius-lg);
    font-size: var(--text-base);
    line-height: var(--leading-relaxed);
    word-wrap: break-word;
  }
  .user-time {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-top: var(--space-1);
  }

  /* ===== 助手消息样式 ===== */
  .message-item.assistant {
    display: flex;
    flex-direction: column;
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    border: 1px solid var(--border);
    margin-right: var(--space-2);  /* 减少右边距，配合 MessageList 的 padding-right 调整 */
    transition: all var(--transition-fast);
  }
  .message-item.streaming { border-color: var(--info); }

  .message-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--space-3);
    font-size: var(--text-sm);
  }
  .message-source {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .message-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .message-time {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }
  .copy-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    opacity: 0;
    transition: all var(--transition-fast);
  }
  .message-item:hover .copy-btn { opacity: 1; }
  .copy-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
  .message-content {
    line-height: var(--leading-relaxed);
    word-wrap: break-word;
    overflow-wrap: break-word;
    font-size: var(--text-base);
  }
  .message-item.streaming .message-content { position: relative; }
  .message-item.streaming .message-content::after {
    content: '';
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 20px;
    background: linear-gradient(transparent, var(--assistant-message-bg));
    pointer-events: none;
    opacity: 0.5;
  }
</style>
