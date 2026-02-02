/**
 * 客户端接口类型定义
 * 
 * @module Client
 * @description 定义 WebSocket 客户端核心接口、配置、事件等类型
 * @author Architecture Team
 * @version 1.0.0
 */

import { WSMessage, MessageType, ConnectionState } from './protocol.types';

/**
 * WebSocket 客户端配置选项
 * {{ AURA: Add - 定义客户端连接配置 }}
 */
export interface WebSocketClientOptions {
  /** WebSocket 服务器地址（如 'ws://localhost:8080'） */
  url: string;
  /** 认证令牌（可选） */
  token?: string;
  /** 心跳间隔（毫秒） */
  heartbeatInterval?: number;
  /** 是否启用自动重连 */
  autoReconnect?: boolean;
  /** 重连配置 */
  reconnect?: ReconnectOptions;
  /** 连接超时时间（毫秒） */
  connectionTimeout?: number;
  /** 消息发送超时（毫秒） */
  messageTimeout?: number;
  /** 消息队列最大长度（离线时缓存） */
  maxQueueSize?: number;
  /** 是否启用消息确认 */
  enableAck?: boolean;
  /** 确认超时时间（毫秒） */
  ackTimeout?: number;
  /** 自定义协议（子协议） */
  protocols?: string | string[];
  /** 额外的 HTTP 头 */
  headers?: Record<string, string>;
  /** 客户端元数据 */
  metadata?: {
    /** 平台标识 */
    platform?: string;
    /** 客户端版本 */
    version?: string;
    /** 设备ID */
    deviceId?: string;
    /** 自定义字段 */
    [key: string]: any;
  };
  /** 日志配置 */
  logging?: {
    /** 日志级别 */
    level?: 'debug' | 'info' | 'warn' | 'error';
    /** 是否启用详细日志 */
    verbose?: boolean;
  };
}

/**
 * 重连配置选项
 * {{ AURA: Add - 定义断线重连策略 }}
 */
export interface ReconnectOptions {
  /** 是否启用重连 */
  enabled?: boolean;
  /** 最大重连次数（0 表示无限） */
  maxAttempts?: number;
  /** 初始延迟（毫秒） */
  initialDelay?: number;
  /** 最大延迟（毫秒） */
  maxDelay?: number;
  /** 延迟增长因子（指数退避） */
  backoffFactor?: number;
  /** 重连策略 */
  strategy?: 'exponential' | 'linear' | 'fixed';
  /** 是否在网络恢复时立即重连 */
  reconnectOnNetworkRestore?: boolean;
}

/**
 * 客户端事件类型枚举
 * {{ AURA: Add - 定义客户端生命周期事件 }}
 */
export enum ClientEvent {
  /** 正在连接 */
  CONNECTING = 'connecting',
  /** 连接成功 */
  CONNECTED = 'connected',
  /** 连接断开 */
  DISCONNECTED = 'disconnected',
  /** 正在重连 */
  RECONNECTING = 'reconnecting',
  /** 重连成功 */
  RECONNECTED = 'reconnected',
  /** 重连失败 */
  RECONNECT_FAILED = 'reconnect_failed',
  /** 收到消息 */
  MESSAGE = 'message',
  /** 消息已发送 */
  MESSAGE_SENT = 'message_sent',
  /** 发送失败 */
  SEND_FAILED = 'send_failed',
  /** 错误发生 */
  ERROR = 'error',
  /** 心跳发送 */
  HEARTBEAT = 'heartbeat',
  /** 心跳响应 */
  HEARTBEAT_ACK = 'heartbeat_ack',
  /** 认证成功 */
  AUTHENTICATED = 'authenticated',
  /** 认证失败 */
  AUTH_FAILED = 'auth_failed',
  /** 状态变更 */
  STATE_CHANGED = 'state_changed',
}

/**
 * 客户端状态信息
 * {{ AURA: Add - 定义客户端运行状态 }}
 */
export interface ClientStatus {
  /** 连接状态 */
  state: ConnectionState;
  /** 连接ID（服务端分配） */
  connectionId?: string;
  /** 连接建立时间 */
  connectedAt?: number;
  /** 连接时长（毫秒） */
  connectionDuration?: number;
  /** 重连次数 */
  reconnectAttempts: number;
  /** 是否已认证 */
  authenticated: boolean;
  /** 待发送消息队列长度 */
  queueSize: number;
  /** 发送消息总数 */
  messagesSent: number;
  /** 接收消息总数 */
  messagesReceived: number;
  /** 上次心跳时间 */
  lastHeartbeat?: number;
  /** 网络延迟（毫秒） */
  latency?: number;
}

/**
 * 消息发送结果
 * {{ AURA: Add - 定义消息发送返回 }}
 */
export interface SendResult {
  /** 是否发送成功 */
  success: boolean;
  /** 消息ID */
  messageId?: string;
  /** 错误信息（如果失败） */
  error?: string;
  /** 发送时间戳 */
  timestamp: number;
}

/**
 * 消息确认结果
 * {{ AURA: Add - 定义消息确认回调 }}
 */
export interface AckResult {
  /** 消息ID */
  messageId: string;
  /** 确认状态 */
  status: 'success' | 'partial' | 'failed';
  /** 附加信息 */
  info?: any;
  /** 耗时（毫秒） */
  duration: number;
}

/**
 * 消息事件处理器类型
 * {{ AURA: Add - 定义消息回调函数签名 }}
 */
export type MessageEventHandler<T = any> = (message: WSMessage<T>) => void | Promise<void>;

/**
 * 事件处理器类型映射
 * {{ AURA: Add - 定义各类事件的处理器签名 }}
 */
export interface ClientEventHandlers {
  [ClientEvent.CONNECTING]: () => void;
  [ClientEvent.CONNECTED]: (connectionId: string) => void;
  [ClientEvent.DISCONNECTED]: (code: number, reason: string) => void;
  [ClientEvent.RECONNECTING]: (attempt: number, delay: number) => void;
  [ClientEvent.RECONNECTED]: (connectionId: string) => void;
  [ClientEvent.RECONNECT_FAILED]: (attempts: number) => void;
  [ClientEvent.MESSAGE]: MessageEventHandler;
  [ClientEvent.MESSAGE_SENT]: (messageId: string) => void;
  [ClientEvent.SEND_FAILED]: (error: string) => void;
  [ClientEvent.ERROR]: (error: Error) => void;
  [ClientEvent.HEARTBEAT]: () => void;
  [ClientEvent.HEARTBEAT_ACK]: (latency: number) => void;
  [ClientEvent.AUTHENTICATED]: (userId: string) => void;
  [ClientEvent.AUTH_FAILED]: (error: string) => void;
  [ClientEvent.STATE_CHANGED]: (oldState: ConnectionState, newState: ConnectionState) => void;
}

/**
 * WebSocket 客户端核心接口
 * {{ AURA: Add - 定义客户端主接口契约 }}
 */
export interface IWebSocketClient {
  /**
   * 建立连接
   */
  connect(): Promise<void>;

  /**
   * 断开连接
   * @param code 关闭码（可选）
   * @param reason 关闭原因（可选）
   */
  disconnect(code?: number, reason?: string): Promise<void>;

  /**
   * 发送消息
   * @param message 消息内容（会自动包装成 WSMessage 格式）
   * @param options 发送选项
   */
  send(message: any, options?: {
    /** 消息类型 */
    type?: MessageType;
    /** 是否需要确认 */
    requireAck?: boolean;
    /** 确认超时（毫秒） */
    ackTimeout?: number;
    /** 优先级 */
    priority?: 'low' | 'normal' | 'high';
  }): Promise<SendResult>;

  /**
   * 发送原始 WSMessage
   * @param message WSMessage 对象
   * @param options 发送选项
   */
  sendMessage(message: WSMessage, options?: {
    requireAck?: boolean;
    ackTimeout?: number;
  }): Promise<SendResult>;

  /**
   * 订阅频道
   * @param channels 频道名称数组
   * @param options 订阅选项
   */
  subscribe(channels: string[], options?: {
    includeHistory?: boolean;
    historyLimit?: number;
  }): Promise<boolean>;

  /**
   * 取消订阅
   * @param channels 频道名称数组
   */
  unsubscribe(channels: string[]): Promise<boolean>;

  /**
   * 加入房间
   * @param roomId 房间ID
   */
  joinRoom(roomId: string): Promise<boolean>;

  /**
   * 离开房间
   * @param roomId 房间ID
   */
  leaveRoom(roomId: string): Promise<boolean>;

  /**
   * 认证
   * @param token 认证令牌
   */
  authenticate(token: string): Promise<boolean>;

  /**
   * 获取客户端状态
   */
  getStatus(): ClientStatus;

  /**
   * 获取连接ID
   */
  getConnectionId(): string | null;

  /**
   * 检查是否已连接
   */
  isConnected(): boolean;

  /**
   * 手动触发重连
   */
  reconnect(): Promise<void>;

  /**
   * 注册消息类型处理器
   * @param messageType 消息类型
   * @param handler 处理函数
   */
  onMessage<T = any>(messageType: MessageType | string, handler: MessageEventHandler<T>): void;

  /**
   * 注册事件监听器
   * @param event 事件类型
   * @param handler 事件处理函数
   */
  on<E extends ClientEvent>(event: E, handler: ClientEventHandlers[E]): void;

  /**
   * 取消事件监听器
   * @param event 事件类型
   * @param handler 事件处理函数
   */
  off<E extends ClientEvent>(event: E, handler: ClientEventHandlers[E]): void;

  /**
   * 注册一次性事件监听器
   * @param event 事件类型
   * @param handler 事件处理函数
   */
  once<E extends ClientEvent>(event: E, handler: ClientEventHandlers[E]): void;

  /**
   * 清空消息队列
   */
  clearQueue(): void;

  /**
   * 销毁客户端实例（释放资源）
   */
  destroy(): void;
}

/**
 * 客户端工厂接口
 * {{ AURA: Add - 定义客户端创建工厂 }}
 */
export interface IWebSocketClientFactory {
  /**
   * 创建客户端实例
   * @param options 客户端配置
   */
  createClient(options: WebSocketClientOptions): IWebSocketClient;
}
