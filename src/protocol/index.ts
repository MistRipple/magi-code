/**
 * 统一消息协议模块导出
 */

export {
  // 枚举
  MessageType,
  MessageLifecycle,
  InteractionType,
  
  // 类型
  type MessageSource,
  type TextBlock,
  type CodeBlock,
  type ThinkingBlock,
  type ToolCallBlock,
  type FileChangeBlock,
  type ContentBlock,
  type InteractionRequest,
  type StandardMessage,
  type MessageMetadata,
  type StreamUpdate,
  
  // 工厂函数
  generateMessageId,
  createStandardMessage,
  createTextMessage,
  createStreamingMessage,
  createErrorMessage,
  createInteractionMessage,
} from './message-protocol';

