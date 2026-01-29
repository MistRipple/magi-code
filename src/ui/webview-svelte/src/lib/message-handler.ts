/**
 * 消息处理器 - 处理来自 VS Code 扩展的消息
 */

import { vscode, type WebviewMessage } from '../lib/vscode-bridge';
import {
  getState,
  addThreadMessage,
  updateThreadMessage,
  clearThreadMessages,
  setIsProcessing,
  setCurrentSessionId,
  updateSessions,
  setAppState,
} from '../stores/messages.svelte';
import {
  startStreaming,
  appendStreamingContent,
  endStreaming,
  createStreamingMessage,
  isMessageStreaming,
} from '../stores/streaming.svelte';
import type { Message, AppState, Session } from '../types/message';

// 生成唯一 ID
function generateId(): string {
  return `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}

/**
 * 初始化消息处理器
 */
export function initMessageHandler() {
  vscode.onMessage(handleMessage);
  console.log('[MessageHandler] 消息处理器已初始化');
}

/**
 * 处理来自扩展的消息
 */
function handleMessage(message: WebviewMessage) {
  const { type } = message;
  
  switch (type) {
    case 'stateSync':
      handleStateSync(message);
      break;
      
    case 'streamStart':
      handleStreamStart(message);
      break;
      
    case 'streamDelta':
      handleStreamDelta(message);
      break;
      
    case 'streamEnd':
      handleStreamEnd(message);
      break;
      
    case 'addMessage':
      handleAddMessage(message);
      break;
      
    case 'updateMessage':
      handleUpdateMessage(message);
      break;
      
    case 'clearMessages':
      handleClearMessages();
      break;
      
    case 'processingStateChange':
      handleProcessingStateChange(message);
      break;
      
    case 'sessionUpdate':
      handleSessionUpdate(message);
      break;
      
    default:
      console.log('[MessageHandler] 未知消息类型:', type, message);
  }
}

// ============ 消息处理函数 ============

function handleStateSync(message: WebviewMessage) {
  const state = message.state as AppState;
  if (state) {
    setAppState(state);
    if (state.sessions) {
      updateSessions(state.sessions as Session[]);
    }
    if (state.currentSession) {
      setCurrentSessionId((state.currentSession as Session).id);
    }
    if (typeof state.isProcessing === 'boolean') {
      setIsProcessing(state.isProcessing);
    }
  }
}

function handleStreamStart(message: WebviewMessage) {
  const messageId = (message.messageId as string) || generateId();
  const source = (message.source as Message['source']) || 'orchestrator';
  
  // 创建新的流式消息
  const streamingMessage = createStreamingMessage(messageId, source);
  addThreadMessage(streamingMessage);
  
  // 开始流式状态追踪
  startStreaming(messageId);
  setIsProcessing(true);
  
  console.log('[MessageHandler] 开始流式输出:', messageId);
}

function handleStreamDelta(message: WebviewMessage) {
  const delta = message.delta as string;
  const messageId = message.messageId as string;
  
  if (!delta) return;
  
  // 追加内容到缓冲区
  const shouldRender = appendStreamingContent(delta);
  
  if (shouldRender) {
    // 获取累积内容并更新消息
    const state = getState();
    const currentMessage = state.threadMessages.find(m => m.id === messageId);
    
    if (currentMessage && isMessageStreaming(messageId)) {
      updateThreadMessage(messageId, {
        content: currentMessage.content + delta,
      });
    }
  }
}

function handleStreamEnd(message: WebviewMessage) {
  const messageId = message.messageId as string;
  const finalContent = endStreaming();
  
  // 更新消息为完成状态
  updateThreadMessage(messageId, {
    content: finalContent || (message.content as string) || '',
    isStreaming: false,
    isComplete: true,
  });
  
  setIsProcessing(false);
  console.log('[MessageHandler] 流式输出结束:', messageId);
}

function handleAddMessage(message: WebviewMessage) {
  const msgData = message.message as Partial<Message>;
  if (!msgData) return;
  
  const newMessage: Message = {
    id: msgData.id || generateId(),
    role: msgData.role || 'assistant',
    source: msgData.source || 'orchestrator',
    content: msgData.content || '',
    timestamp: msgData.timestamp || Date.now(),
    isStreaming: false,
    isComplete: true,
    metadata: msgData.metadata,
  };
  
  addThreadMessage(newMessage);
}

function handleUpdateMessage(message: WebviewMessage) {
  const messageId = message.messageId as string;
  const updates = message.updates as Partial<Message>;
  
  if (messageId && updates) {
    updateThreadMessage(messageId, updates);
  }
}

function handleClearMessages() {
  clearThreadMessages();
}

function handleProcessingStateChange(message: WebviewMessage) {
  const isProcessing = message.isProcessing as boolean;
  setIsProcessing(isProcessing);
}

function handleSessionUpdate(message: WebviewMessage) {
  const sessions = message.sessions as Session[];
  const currentSessionId = message.currentSessionId as string;
  
  if (sessions) {
    updateSessions(sessions);
  }
  if (currentSessionId) {
    setCurrentSessionId(currentSessionId);
  }
}

