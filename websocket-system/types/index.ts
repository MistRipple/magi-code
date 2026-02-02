/**
 * WebSocket 系统类型定义统一导出
 * 
 * @module Types
 * @description 提供所有 WebSocket 相关类型的统一入口
 * @author Architecture Team
 * @version 1.0.0
 */

// ========== 协议相关类型 ==========
export {
  // 枚举
  MessageType,
  ErrorCode,
  ConnectionState,
  
  // 接口
  MessageHeader,
  MessagePayload,
  ErrorPayload,
  AckPayload,
  WSMessage,
  HeartbeatData,
  HeartbeatAckData,
  ConnectData,
  ConnectAckData,
  SubscribeData,
  RoomData,
  
  // 常量
  PROTOCOL_VERSION,
  MAX_MESSAGE_SIZE,
  DEFAULT_HEARTBEAT_CONFIG,
  DEFAULT_RECONNECT_CONFIG,
  
  // 类型守卫
  isErrorPayload,
  isAckPayload,
  isMessagePayload,
} from './protocol.types';

// ========== 连接管理相关类型 ==========
export {
  // 接口
  ConnectionMetadata,
  ConnectionInfo,
  ConnectionPoolStats,
  ConnectionManagerOptions,
  ConnectionFilter,
  ConnectionOperationResult,
  BatchConnectionOperationResult,
  
  // 枚举
  ConnectionEvent,
  
  // 类型别名
  ConnectionEventHandler,
} from './connection.types';

// ========== 服务端相关类型 ==========
export {
  // 接口
  WebSocketServerOptions,
  ServerStats,
  SendMessageOptions,
  BroadcastOptions,
  ChannelManager,
  RoomManager,
  AuthHandler,
  MessageRouter,
  MessageContext,
  IWebSocketServer,
  
  // 枚举
  ServerEvent,
  
  // 类型别名
  MessageHandler,
} from './server.types';

// ========== 客户端相关类型 ==========
export {
  // 接口
  WebSocketClientOptions,
  ReconnectOptions,
  ClientStatus,
  SendResult,
  AckResult,
  ClientEventHandlers,
  IWebSocketClient,
  IWebSocketClientFactory,
  
  // 枚举
  ClientEvent,
  
  // 类型别名
  MessageEventHandler,
} from './client.types';

/**
 * 版本信息
 * {{ AURA: Add - 提供类型系统版本信息 }}
 */
export const TYPES_VERSION = '1.0.0';

/**
 * 类型系统元数据
 * {{ AURA: Add - 提供类型系统元信息 }}
 */
export const TYPE_SYSTEM_META = {
  version: TYPES_VERSION,
  protocolVersion: '1.0.0',
  compatibility: {
    minServerVersion: '1.0.0',
    minClientVersion: '1.0.0',
  },
  features: [
    'message-protocol',
    'connection-management',
    'heartbeat-detection',
    'auto-reconnect',
    'channel-subscription',
    'room-management',
    'authentication',
    'message-routing',
  ],
} as const;
