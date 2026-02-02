/**
 * 服务端接口类型定义
 * 
 * @module Server
 * @description 定义 WebSocket 服务端核心接口、配置、事件等类型
 * @author Architecture Team
 * @version 1.0.0
 */

import { WSMessage, MessageType } from './protocol.types';
import { ConnectionInfo, ConnectionFilter, BatchConnectionOperationResult } from './connection.types';

/**
 * WebSocket 服务端配置选项
 * {{ AURA: Add - 定义服务端启动配置 }}
 */
export interface WebSocketServerOptions {
  /** 监听端口 */
  port?: number;
  /** 监听主机地址 */
  host?: string;
  /** 路径（默认为 '/'） */
  path?: string;
  /** 最大连接数 */
  maxConnections?: number;
  /** 心跳检测间隔（毫秒） */
  heartbeatInterval?: number;
  /** 心跳超时时间（毫秒） */
  heartbeatTimeout?: number;
  /** 最大消息大小（字节） */
  maxMessageSize?: number;
  /** 是否启用压缩 */
  compression?: boolean;
  /** 是否需要认证 */
  requireAuth?: boolean;
  /** 认证超时时间（毫秒） */
  authTimeout?: number;
  /** 跨域配置 */
  cors?: {
    /** 允许的源 */
    origin?: string | string[];
    /** 允许的凭证 */
    credentials?: boolean;
  };
  /** SSL/TLS 配置（可选） */
  ssl?: {
    /** 证书文件路径 */
    cert?: string;
    /** 私钥文件路径 */
    key?: string;
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
 * 服务端事件类型枚举
 * {{ AURA: Add - 定义服务端生命周期事件 }}
 */
export enum ServerEvent {
  /** 服务器启动 */
  STARTED = 'started',
  /** 服务器停止 */
  STOPPED = 'stopped',
  /** 新连接建立 */
  CONNECTION = 'connection',
  /** 连接断开 */
  DISCONNECTION = 'disconnection',
  /** 收到消息 */
  MESSAGE = 'message',
  /** 发送消息 */
  MESSAGE_SENT = 'message_sent',
  /** 错误发生 */
  ERROR = 'error',
  /** 心跳超时 */
  HEARTBEAT_TIMEOUT = 'heartbeat_timeout',
  /** 认证成功 */
  AUTHENTICATED = 'authenticated',
  /** 认证失败 */
  AUTH_FAILED = 'auth_failed',
}

/**
 * 服务端统计信息
 * {{ AURA: Add - 定义服务端运行状态监控数据 }}
 */
export interface ServerStats {
  /** 服务器启动时间 */
  startTime: number;
  /** 运行时长（毫秒） */
  uptime: number;
  /** 监听端口 */
  port: number;
  /** 当前连接数 */
  currentConnections: number;
  /** 峰值连接数 */
  peakConnections: number;
  /** 累计连接数 */
  totalConnections: number;
  /** 累计发送消息数 */
  totalMessagesSent: number;
  /** 累计接收消息数 */
  totalMessagesReceived: number;
  /** 累计错误数 */
  totalErrors: number;
  /** 平均消息处理时间（毫秒） */
  averageMessageProcessingTime: number;
  /** 内存使用情况 */
  memory?: {
    /** 堆内存使用（字节） */
    heapUsed?: number;
    /** 堆内存总量（字节） */
    heapTotal?: number;
  };
}

/**
 * 消息发送选项
 * {{ AURA: Add - 定义消息发送配置 }}
 */
export interface SendMessageOptions {
  /** 是否需要确认 */
  requireAck?: boolean;
  /** 确认超时时间（毫秒） */
  ackTimeout?: number;
  /** 消息优先级 */
  priority?: 'low' | 'normal' | 'high';
  /** 是否压缩 */
  compress?: boolean;
  /** 重试次数 */
  retries?: number;
  /** 超时时间（毫秒） */
  timeout?: number;
}

/**
 * 广播消息选项
 * {{ AURA: Add - 定义广播配置 }}
 */
export interface BroadcastOptions extends SendMessageOptions {
  /** 排除的连接ID列表 */
  excludeIds?: string[];
  /** 只发送给指定频道的订阅者 */
  channel?: string;
  /** 只发送给指定房间的成员 */
  roomId?: string;
  /** 连接过滤器 */
  filter?: ConnectionFilter;
}

/**
 * 频道管理接口
 * {{ AURA: Add - 定义频道订阅管理 }}
 */
export interface ChannelManager {
  /**
   * 创建频道
   * @param channelName 频道名称
   * @param metadata 频道元数据
   */
  createChannel(channelName: string, metadata?: Record<string, any>): Promise<boolean>;

  /**
   * 删除频道
   * @param channelName 频道名称
   */
  deleteChannel(channelName: string): Promise<boolean>;

  /**
   * 订阅频道
   * @param connectionId 连接ID
   * @param channelName 频道名称
   */
  subscribe(connectionId: string, channelName: string): Promise<boolean>;

  /**
   * 取消订阅
   * @param connectionId 连接ID
   * @param channelName 频道名称
   */
  unsubscribe(connectionId: string, channelName: string): Promise<boolean>;

  /**
   * 获取频道订阅者列表
   * @param channelName 频道名称
   */
  getSubscribers(channelName: string): string[];

  /**
   * 获取所有频道列表
   */
  getAllChannels(): string[];

  /**
   * 向频道广播消息
   * @param channelName 频道名称
   * @param message 消息内容
   * @param options 发送选项
   */
  broadcast(channelName: string, message: WSMessage, options?: SendMessageOptions): Promise<BatchConnectionOperationResult>;
}

/**
 * 房间管理接口
 * {{ AURA: Add - 定义房间管理 }}
 */
export interface RoomManager {
  /**
   * 创建房间
   * @param roomId 房间ID
   * @param options 房间配置
   */
  createRoom(roomId: string, options?: {
    maxMembers?: number;
    metadata?: Record<string, any>;
  }): Promise<boolean>;

  /**
   * 删除房间
   * @param roomId 房间ID
   */
  deleteRoom(roomId: string): Promise<boolean>;

  /**
   * 加入房间
   * @param connectionId 连接ID
   * @param roomId 房间ID
   */
  join(connectionId: string, roomId: string): Promise<boolean>;

  /**
   * 离开房间
   * @param connectionId 连接ID
   * @param roomId 房间ID
   */
  leave(connectionId: string, roomId: string): Promise<boolean>;

  /**
   * 获取房间成员列表
   * @param roomId 房间ID
   */
  getMembers(roomId: string): string[];

  /**
   * 获取所有房间列表
   */
  getAllRooms(): string[];

  /**
   * 向房间广播消息
   * @param roomId 房间ID
   * @param message 消息内容
   * @param options 发送选项
   */
  broadcast(roomId: string, message: WSMessage, options?: SendMessageOptions): Promise<BatchConnectionOperationResult>;
}

/**
 * 认证处理器接口
 * {{ AURA: Add - 定义认证逻辑接口 }}
 */
export interface AuthHandler {
  /**
   * 验证认证信息
   * @param token 认证令牌
   * @param metadata 额外的认证数据
   * @returns 认证结果，包含用户信息
   */
  authenticate(token: string, metadata?: Record<string, any>): Promise<{
    success: boolean;
    userId?: string;
    roles?: string[];
    permissions?: string[];
    error?: string;
  }>;

  /**
   * 刷新令牌
   * @param oldToken 旧令牌
   * @returns 新令牌
   */
  refreshToken?(oldToken: string): Promise<string | null>;

  /**
   * 撤销令牌
   * @param token 令牌
   */
  revokeToken?(token: string): Promise<boolean>;
}

/**
 * 消息路由器接口
 * {{ AURA: Add - 定义消息路由处理 }}
 */
export interface MessageRouter {
  /**
   * 注册消息处理器
   * @param messageType 消息类型
   * @param handler 处理函数
   */
  registerHandler(
    messageType: MessageType | string,
    handler: MessageHandler
  ): void;

  /**
   * 取消注册消息处理器
   * @param messageType 消息类型
   */
  unregisterHandler(messageType: MessageType | string): void;

  /**
   * 路由消息到对应的处理器
   * @param connectionId 连接ID
   * @param message 消息内容
   */
  route(connectionId: string, message: WSMessage): Promise<void>;
}

/**
 * 消息处理器函数类型
 * {{ AURA: Add - 定义消息处理函数签名 }}
 */
export type MessageHandler = (
  connectionId: string,
  message: WSMessage,
  context: MessageContext
) => void | Promise<void>;

/**
 * 消息处理上下文
 * {{ AURA: Add - 定义消息处理时的上下文信息 }}
 */
export interface MessageContext {
  /** 连接信息 */
  connection: ConnectionInfo;
  /** 服务器实例引用 */
  server: IWebSocketServer;
  /** 发送响应的快捷方法 */
  reply: (data: any) => Promise<boolean>;
  /** 发送错误的快捷方法 */
  error: (code: number, message: string, details?: any) => Promise<boolean>;
  /** 发送确认的快捷方法 */
  ack: (status: 'success' | 'partial' | 'failed', info?: any) => Promise<boolean>;
}

/**
 * WebSocket 服务端核心接口
 * {{ AURA: Add - 定义服务端主接口契约 }}
 */
export interface IWebSocketServer {
  /**
   * 启动服务器
   */
  start(): Promise<void>;

  /**
   * 停止服务器
   */
  stop(): Promise<void>;

  /**
   * 发送消息给指定客户端（单播）
   * @param connectionId 连接ID
   * @param message 消息内容
   * @param options 发送选项
   */
  sendToClient(connectionId: string, message: WSMessage | any, options?: SendMessageOptions): Promise<boolean>;

  /**
   * 广播消息给所有或部分客户端
   * @param message 消息内容
   * @param options 广播选项
   */
  broadcast(message: WSMessage | any, options?: BroadcastOptions): Promise<BatchConnectionOperationResult>;

  /**
   * 获取连接信息
   * @param connectionId 连接ID
   */
  getConnection(connectionId: string): ConnectionInfo | null;

  /**
   * 获取所有连接
   * @param filter 过滤条件
   */
  getAllConnections(filter?: ConnectionFilter): ConnectionInfo[];

  /**
   * 断开指定连接
   * @param connectionId 连接ID
   * @param code 关闭码
   * @param reason 关闭原因
   */
  disconnect(connectionId: string, code?: number, reason?: string): Promise<boolean>;

  /**
   * 获取服务器统计信息
   */
  getStats(): ServerStats;

  /**
   * 注册事件监听器
   * @param event 事件类型
   * @param handler 事件处理函数
   */
  on(event: ServerEvent | string, handler: (...args: any[]) => void): void;

  /**
   * 取消事件监听器
   * @param event 事件类型
   * @param handler 事件处理函数
   */
  off(event: ServerEvent | string, handler: (...args: any[]) => void): void;

  /**
   * 频道管理器
   */
  readonly channels: ChannelManager;

  /**
   * 房间管理器
   */
  readonly rooms: RoomManager;

  /**
   * 消息路由器
   */
  readonly router: MessageRouter;
}
