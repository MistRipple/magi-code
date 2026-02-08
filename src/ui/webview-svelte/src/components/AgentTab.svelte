<script lang="ts">
  import type { Message } from '../types/message';
  import MessageList from './MessageList.svelte';

  // Props
  interface Props {
    messages: Message[];
  }

  let { messages }: Props = $props();

  // Worker Tab 专用的空状态配置
  const emptyState = {
    icon: 'message-square',
    title: '暂无输出',
    hint: '此 Worker 尚未产生任何消息'
  };
</script>

<!-- 复用 MessageList 组件，displayContext='worker' 标识 Worker 面板 -->
<MessageList {messages} {emptyState} displayContext="worker" />
