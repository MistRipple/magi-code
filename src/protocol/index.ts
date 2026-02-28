/**
 * 统一消息协议模块导出
 */

export {
  // 枚举
  MessageType,
  MessageLifecycle,
  InteractionType,
  // 🔧 统一消息通道（unified-message-channel-design.md v2.5）
  MessageCategory,
  ControlMessageType,

  // 类型
  type MessageSource,
  type NotifyLevel,
  type DataMessageType,
  type TextBlock,
  type CodeBlock,
  type ThinkingBlock,
  type StandardizedToolStatus,
  type StandardizedToolResultPayload,
  type ToolCallBlock,
  type FileChangeBlock,
  type ContentBlock,
  type InteractionRequest,
  type StandardMessage,
  type MessageMetadata,
  type StreamUpdate,
  type ControlPayload,
  type NotifyPayload,
  type DataPayload,

  // 工厂函数
  generateMessageId,
  createStandardMessage,
  createTextMessage,
  createStreamingMessage,
  createErrorMessage,
  createInteractionMessage,
  // 🔧 统一消息通道工厂函数
  createControlMessage,
  createNotifyMessage,
  createDataMessage,
} from './message-protocol';
