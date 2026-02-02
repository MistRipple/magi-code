/**
 * WebSocket 实时消息推送系统 - 核心接口定义
 * 
 * @description 本文件定义了服务端和客户端的所有核心接口、类型和枚举
 * @version 1.0.0
 * @author Antigravity (Google Deepmind)
 */

// ============================================================================
// 基础类型定义
// ============================================================================

/**
 * 日志器接口
 */
export interface Logger {
  debug(message: string, ...args: any[]): void;
  info(message: string, ...args: any[]): void;
  warn(message: string, ...args: any[]): void;
  error(message: string, ...args: any[]): void;
}

// ============================================================================
// 消息协议类型定义
// ============================================================================

/**
 * 客户端消息类型枚举
 */
export enum ClientMessageType {
  // 连接控制
  PING = 'ping',                          // 心跳 Ping
  AUTH = 'auth',                          // 身份认证
  
  // 房间操作
  JOIN_ROOM = 'join_room',                // 加入房间
  LEAVE_ROOM = 'leave_room',              // 离开房间
  CREATE_ROOM = 'create_room',            // 创建房间
  
  // 消息发送
  SEND_MESSAGE = 'send_message',          // 发送消息（点对点或房间）
  BROADCAST = 'broadcast',                // 广播消息
  
  // 查询操作
  GET_ROOM_MEMBERS = 'get_room_members',  // 获取房间成员
  GET_USER_ROOMS = 'get_user_rooms',      // 获取用户所在房间
  
  // 自定义
  CUSTOM = 'custom',                      // 自定义消息类型
}

/**
 * 服务端消息类型枚举
 */
export enum ServerMessageType {
  // 连接控制
  PONG = 'pong',                          // 心跳 Pong
  AUTH_SUCCESS = 'auth_success',          // 认证成功
  AUTH_FAILURE = 'auth_failure',          // 认证失败
  
  // 房间通知
  ROOM_JOINED = 'room_joined',            // 已加入房间
  ROOM_LEFT = 'room_left',                // 已离开房间
  ROOM_CREATED = 'room_created',          // 房间已创建
  USER_JOINED = 'user_joined',            // 用户加入（房间内广播）
  USER_LEFT = 'user_left',                // 用户离开（房间内广播）
  
  // 消息接收
  MESSAGE = 'message',                    // 接收消息（点对点或房间）
  BROADCAST = 'broadcast',                // 接收广播消息
  
  // 查询响应
  ROOM_MEMBERS = 'room_members',          // 房间成员列表
  USER_ROOMS = 'user_rooms',              // 用户房间列表
  
  // 错误和通知
  ERROR = 'error',                        // 错误消息
  NOTIFICATION = 'notification',          // 系统通知
  
  // 自定义
  CUSTOM = 'custom',                      // 自定义消息类型
}

/**
 * 客户端发送的消息格式
 */
export interface ClientMessage {
  /** 消息唯一ID（客户端生成，用于追踪和去重） */
  id: string;
  
  /** 消息类型 */
  type: ClientMessageType;
  
  /** 协议版本 */
  version?: string;
  
  /** 时间戳（客户端发送时间） */
  timestamp: number;
  
  /** 消息负载（根据 type 不同而不同） */
  payload: any;
  
  /** 元数据（可选） */
  metadata?: {
    userId?: string;
    deviceId?: string;
    [key: string]: any;
  };
}

/**
 * 服务端发送的消息格式
 */
export interface ServerMessage {
  /** 消息唯一ID（服务端生成） */
  id: string;
  
  /** 消息类型 */
  type: ServerMessageType;
  
  /** 协议版本 */
  version?: string;
  
  /** 时间戳（服务端发送时间） */
  timestamp: number;
  
  /** 消息负载 */
  payload: any;
  
  /** 关联的客户端消息ID（用于响应） */
  replyTo?: string;
  
  /** 元数据 */
  metadata?: {
    fromUserId?: string;
    roomId?: string;
    [key: string]: any;
  };
}

// ============================================================================
// 消息 Payload 类型定义
// ============================================================================

// 客户端 Payload

export interface PingPayload {
  /** Echo 数据（可选） */
  echo?: any;
}

export interface AuthPayload {
  /** 认证令牌 */
  token?: string;
  /** 用户ID */
  userId?: string;
  /** 其他认证信息 */
  credentials?: any;
}

export interface JoinRoomPayload {
  /** 房间ID */
  roomId: string;
  /** 房间密码（私密房间） */
  password?: string;
}

export interface LeaveRoomPayload {
  /** 房间ID */
  roomId: string;
}

export interface CreateRoomPayload {
  /** 房间ID */
  roomId: string;
  /** 房间名称 */
  name?: string;
  /** 房间描述 */
  description?: string;
  /** 最大成员数 */
  maxMembers?: number;
  /** 是否私密 */
  isPrivate?: boolean;
  /** 房间密码 */
  password?: string;
}

export interface SendMessagePayload {
  /** 目标类型 */
  targetType: 'user' | 'room';
  /** 目标ID（userId 或 roomId） */
  targetId: string;
  /** 消息内容（文本、JSON、二进制等） */
  content: any;
  /** 内容类型（text, json, image, etc.） */
  contentType?: string;
}

export interface GetRoomMembersPayload {
  /** 房间ID */
  roomId: string;
}

export interface GetUserRoomsPayload {
  /** 用户ID（空则查询自己） */
  userId?: string;
}

// 服务端 Payload

export interface PongPayload {
  /** 服务端时间戳 */
  timestamp: number;
  /** Echo 回显数据 */
  echo?: any;
}

export interface AuthSuccessPayload {
  /** 用户ID */
  userId: string;
  /** 连接ID */
  connectionId: string;
  /** 会话数据 */
  sessionData?: any;
}

export interface AuthFailurePayload {
  /** 失败原因 */
  reason: string;
  /** 错误码 */
  code?: string;
}

export interface RoomJoinedPayload {
  /** 房间ID */
  roomId: string;
  /** 房间成员列表 */
  members: RoomMember[];
  /** 房间元数据 */
  metadata?: RoomMetadata;
}

export interface RoomLeftPayload {
  /** 房间ID */
  roomId: string;
}

export interface RoomCreatedPayload {
  /** 房间ID */
  roomId: string;
  /** 房间元数据 */
  metadata: RoomMetadata;
}

export interface UserJoinedPayload {
  /** 房间ID */
  roomId: string;
  /** 用户ID */
  userId: string;
  /** 加入时间 */
  joinedAt: number;
}

export interface UserLeftPayload {
  /** 房间ID */
  roomId: string;
  /** 用户ID */
  userId: string;
  /** 离开时间 */
  leftAt: number;
}

export interface MessagePayload {
  /** 消息ID */
  messageId: string;
  /** 发送者用户ID */
  fromUserId: string;
  /** 目标类型 */
  targetType: 'user' | 'room';
  /** 目标ID */
  targetId: string;
  /** 消息内容 */
  content: any;
  /** 内容类型 */
  contentType?: string;
  /** 发送时间 */
  sentAt: number;
}

export interface BroadcastPayload {
  /** 广播内容 */
  content: any;
  /** 发送者（系统广播可为空） */
  fromUserId?: string;
  /** 发送时间 */
  sentAt: number;
}

export interface RoomMembersPayload {
  /** 房间ID */
  roomId: string;
  /** 成员列表 */
  members: RoomMember[];
}

export interface UserRoomsPayload {
  /** 用户ID */
  userId: string;
  /** 房间列表 */
  rooms: Array<{
    roomId: string;
    name?: string;
    joinedAt: number;
  }>;
}

export interface ErrorPayload {
  /** 错误码 */
  code: string;
  /** 错误描述 */
  message: string;
  /** 错误详情 */
  details?: any;
}

export interface NotificationPayload {
  /** 通知级别 */
  level: 'info' | 'warning' | 'error';
  /** 通知标题 */
  title?: string;
  /** 通知内容 */
  message: string;
  /** 建议操作 */
  action?: string;
}

// ============================================================================
// 连接管理相关类型定义
// ============================================================================

/**
 * 连接元数据
 */
export interface ConnectionMetadata {
  /** 连接ID */
  connectionId: string;
  /** 用户ID */
  userId?: string;
  /** 连接时间 */
  connectedAt: number;
  /** 最后心跳时间 */
  lastHeartbeat: number;
  /** 是否存活 */
  isAlive: boolean;
  /** 用户代理 */
  userAgent?: string;
  /** IP地址 */
  ipAddress?: string;
  /** 自定义数据 */
  customData?: Record<string, any>;
}

/**
 * 连接管理器接口
 */
export interface IConnectionManager {
  /** 注册新连接 */
  registerConnection(connectionId: string, ws: any, userId?: string): void;
  
  /** 注销连接 */
  unregisterConnection(connectionId: string): void;
  
  /** 根据连接ID获取WebSocket实例 */
  getConnection(connectionId: string): any | undefined;
  
  /** 根据用户ID获取所有连接ID */
  getConnectionsByUserId(userId: string): string[];
  
  /** 获取连接元数据 */
  getConnectionMetadata(connectionId: string): ConnectionMetadata | undefined;
  
  /** 更新连接元数据 */
  updateConnectionMetadata(connectionId: string, metadata: Partial<ConnectionMetadata>): void;
  
  /** 获取所有活跃连接 */
  getAllConnections(): Map<string, any>;
  
  /** 启动心跳检测 */
  startHeartbeat(interval?: number): void;
  
  /** 停止心跳检测 */
  stopHeartbeat(): void;
  
  /** 检查连接是否存活 */
  isAlive(connectionId: string): boolean;
}

// ============================================================================
// 消息路由相关类型定义
// ============================================================================

/**
 * 消息处理器函数类型
 */
export type MessageHandler = (message: ClientMessage, connectionId: string) => Promise<void> | void;

/**
 * 消息路由器接口
 */
export interface IMessageRouter {
  /** 路由消息到目标处理器 */
  route(message: ServerMessage, sourceConnectionId?: string): Promise<void>;
  
  /** 广播消息给所有连接 */
  broadcast(message: ServerMessage, excludeConnectionIds?: string[]): Promise<void>;
  
  /** 发送点对点消息 */
  sendToUser(userId: string, message: ServerMessage): Promise<void>;
  
  /** 发送消息到指定连接 */
  sendToConnection(connectionId: string, message: ServerMessage): Promise<void>;
  
  /** 发送消息到房间 */
  sendToRoom(roomId: string, message: ServerMessage, excludeConnectionIds?: string[]): Promise<void>;
  
  /** 注册消息处理器 */
  registerHandler(messageType: ClientMessageType, handler: MessageHandler): void;
  
  /** 注销消息处理器 */
  unregisterHandler(messageType: ClientMessageType): void;
}

// ============================================================================
// 房间管理相关类型定义
// ============================================================================

/**
 * 房间元数据
 */
export interface RoomMetadata {
  /** 房间ID */
  roomId: string;
  /** 房间名称 */
  name?: string;
  /** 房间描述 */
  description?: string;
  /** 创建时间 */
  createdAt: number;
  /** 创建者 */
  createdBy?: string;
  /** 最大成员数 */
  maxMembers?: number;
  /** 是否私密 */
  isPrivate?: boolean;
  /** 自定义数据 */
  customData?: Record<string, any>;
}

/**
 * 房间成员
 */
export interface RoomMember {
  /** 用户ID */
  userId: string;
  /** 连接ID列表（支持多端） */
  connectionIds: string[];
  /** 加入时间 */
  joinedAt: number;
  /** 成员角色 */
  role?: 'owner' | 'admin' | 'member';
}

/**
 * 房间
 */
export interface Room {
  /** 房间元数据 */
  metadata: RoomMetadata;
  /** 房间成员 */
  members: Map<string, RoomMember>;
}

/**
 * 房间管理器接口
 */
export interface IRoomManager {
  /** 创建房间 */
  createRoom(roomId: string, metadata?: RoomMetadata): void;
  
  /** 删除房间 */
  deleteRoom(roomId: string): void;
  
  /** 用户加入房间 */
  joinRoom(roomId: string, userId: string, connectionId: string): void;
  
  /** 用户离开房间 */
  leaveRoom(roomId: string, userId: string, connectionId?: string): void;
  
  /** 获取房间成员列表 */
  getRoomMembers(roomId: string): RoomMember[];
  
  /** 获取房间元数据 */
  getRoomMetadata(roomId: string): RoomMetadata | undefined;
  
  /** 更新房间元数据 */
  updateRoomMetadata(roomId: string, metadata: Partial<RoomMetadata>): void;
  
  /** 获取用户加入的所有房间 */
  getUserRooms(userId: string): string[];
  
  /** 检查房间是否存在 */
  roomExists(roomId: string): boolean;
  
  /** 检查用户是否在房间中 */
  isUserInRoom(roomId: string, userId: string): boolean;
  
  /** 获取所有房间 */
  getAllRooms(): Map<string, Room>;
}

// ============================================================================
// 事件分发相关类型定义
// ============================================================================

/**
 * 系统事件类型枚举
 */
export enum SystemEventType {
  // 连接事件
  CONNECTION_OPENED = 'connection:opened',
  CONNECTION_CLOSED = 'connection:closed',
  CONNECTION_ERROR = 'connection:error',
  HEARTBEAT_TIMEOUT = 'heartbeat:timeout',
  
  // 消息事件
  MESSAGE_RECEIVED = 'message:received',
  MESSAGE_SENT = 'message:sent',
  MESSAGE_ERROR = 'message:error',
  
  // 房间事件
  ROOM_CREATED = 'room:created',
  ROOM_DELETED = 'room:deleted',
  USER_JOINED_ROOM = 'user:joined_room',
  USER_LEFT_ROOM = 'user:left_room',
  
  // 系统事件
  SERVER_STARTED = 'server:started',
  SERVER_STOPPED = 'server:stopped',
  SERVER_ERROR = 'server:error',
}

/**
 * 事件监听器函数类型
 */
export type EventListener = (data: any) => void | Promise<void>;

/**
 * 事件分发器接口
 */
export interface IEventDispatcher {
  /** 注册事件监听器 */
  on(eventType: SystemEventType, listener: EventListener): void;
  
  /** 注册一次性事件监听器 */
  once(eventType: SystemEventType, listener: EventListener): void;
  
  /** 注销事件监听器 */
  off(eventType: SystemEventType, listener?: EventListener): void;
  
  /** 触发事件 */
  emit(eventType: SystemEventType, data: any): void;
  
  /** 获取事件监听器数量 */
  listenerCount(eventType: SystemEventType): number;
}

// ============================================================================
// 服务端相关类型定义
// ============================================================================

/**
 * 认证处理器函数类型
 */
export type AuthHandler = (payload: AuthPayload, connectionId: string) => Promise<AuthResult>;

/**
 * 认证结果
 */
export interface AuthResult {
  /** 是否成功 */
  success: boolean;
  /** 用户ID */
  userId?: string;
  /** 会话数据 */
  sessionData?: any;
  /** 失败原因 */
  reason?: string;
}

/**
 * 服务器配置
 */
export interface ServerConfig {
  /** 服务器端口（默认 8080） */
  port?: number;
  /** 监听地址（默认 '0.0.0.0'） */
  host?: string;
  /** 心跳间隔（毫秒，默认 30000） */
  heartbeatInterval?: number;
  /** 心跳超时（毫秒，默认 60000） */
  heartbeatTimeout?: number;
  /** 最大连接数（默认无限制） */
  maxConnections?: number;
  /** 最大房间数（默认无限制） */
  maxRooms?: number;
  /** 是否启用认证（默认 false） */
  enableAuth?: boolean;
  /** 自定义认证处理器 */
  authHandler?: AuthHandler;
  /** 日志器 */
  logger?: Logger;
  /** 是否启用压缩（默认 false） */
  compression?: boolean;
}

/**
 * 服务器状态
 */
export interface ServerStatus {
  /** 是否运行中 */
  isRunning: boolean;
  /** 端口 */
  port?: number;
  /** 监听地址 */
  host?: string;
  /** 启动时间 */
  startedAt?: number;
  /** 当前连接数 */
  totalConnections: number;
  /** 当前房间数 */
  totalRooms: number;
}

/**
 * WebSocket 服务器接口
 */
export interface IWebSocketServer {
  /** 启动服务器 */
  start(port?: number, host?: string): Promise<void>;
  
  /** 停止服务器 */
  stop(): Promise<void>;
  
  /** 获取服务器状态 */
  getStatus(): ServerStatus;
  
  /** 获取连接管理器 */
  getConnectionManager(): IConnectionManager;
  
  /** 获取消息路由器 */
  getMessageRouter(): IMessageRouter;
  
  /** 获取房间管理器 */
  getRoomManager(): IRoomManager;
  
  /** 获取事件分发器 */
  getEventDispatcher(): IEventDispatcher;
  
  /** 配置服务器 */
  configure(config: ServerConfig): void;
}

// ============================================================================
// 客户端相关类型定义
// ============================================================================

/**
 * 客户端配置选项
 */
export interface ClientOptions {
  /** 自动重连（默认 true） */
  autoReconnect?: boolean;
  /** 重连间隔（毫秒，默认 3000） */
  reconnectInterval?: number;
  /** 最大重连次数（默认 10） */
  maxReconnectAttempts?: number;
  /** 心跳间隔（毫秒，默认 30000） */
  heartbeatInterval?: number;
  /** 请求超时（毫秒，默认 10000） */
  timeout?: number;
  /** 日志器 */
  logger?: Logger;
}

/**
 * 客户端状态
 */
export interface ClientStatus {
  /** 是否已连接 */
  isConnected: boolean;
  /** 连接ID */
  connectionId?: string;
  /** 用户ID */
  userId?: string;
  /** 连接时间 */
  connectedAt?: number;
  /** 重连尝试次数 */
  reconnectAttempts: number;
}

/**
 * 客户端事件处理器函数类型
 */
export type ClientEventHandler = (data: any) => void;

/**
 * WebSocket 客户端接口
 */
export interface IWebSocketClient {
  /** 连接到服务器 */
  connect(url: string, options?: ClientOptions): Promise<void>;
  
  /** 断开连接 */
  disconnect(): void;
  
  /** 发送消息 */
  send(message: ClientMessage): Promise<void>;
  
  /** 监听消息 */
  on(messageType: ServerMessageType | 'open' | 'close' | 'error', handler: ClientEventHandler): void;
  
  /** 取消监听 */
  off(messageType: ServerMessageType | 'open' | 'close' | 'error', handler?: ClientEventHandler): void;
  
  /** 获取连接状态 */
  getStatus(): ClientStatus;
  
  // 便捷方法
  
  /** 发送心跳 */
  ping(): Promise<void>;
  
  /** 认证 */
  auth(payload: AuthPayload): Promise<AuthSuccessPayload>;
  
  /** 加入房间 */
  joinRoom(roomId: string, password?: string): Promise<RoomJoinedPayload>;
  
  /** 离开房间 */
  leaveRoom(roomId: string): Promise<void>;
  
  /** 发送消息 */
  sendMessage(targetType: 'user' | 'room', targetId: string, content: any): Promise<void>;
  
  /** 获取房间成员 */
  getRoomMembers(roomId: string): Promise<RoomMember[]>;
  
  /** 获取用户房间列表 */
  getUserRooms(): Promise<string[]>;
}

// ============================================================================
// 错误码常量定义
// ============================================================================

export const ErrorCodes = {
  // 连接错误 (1xxx)
  CONNECTION_FAILED: 'E1001',
  CONNECTION_TIMEOUT: 'E1002',
  CONNECTION_CLOSED: 'E1003',
  HEARTBEAT_TIMEOUT: 'E1004',
  
  // 认证错误 (2xxx)
  AUTH_REQUIRED: 'E2001',
  AUTH_FAILED: 'E2002',
  AUTH_TOKEN_INVALID: 'E2003',
  AUTH_TOKEN_EXPIRED: 'E2004',
  PERMISSION_DENIED: 'E2005',
  
  // 消息错误 (3xxx)
  MESSAGE_INVALID: 'E3001',
  MESSAGE_TOO_LARGE: 'E3002',
  MESSAGE_TYPE_UNKNOWN: 'E3003',
  MESSAGE_PARSE_ERROR: 'E3004',
  
  // 房间错误 (4xxx)
  ROOM_NOT_FOUND: 'E4001',
  ROOM_FULL: 'E4002',
  ROOM_PASSWORD_REQUIRED: 'E4003',
  ROOM_PASSWORD_INCORRECT: 'E4004',
  ROOM_ALREADY_EXISTS: 'E4005',
  USER_NOT_IN_ROOM: 'E4006',
  USER_ALREADY_IN_ROOM: 'E4007',
  
  // 用户错误 (5xxx)
  USER_NOT_FOUND: 'E5001',
  USER_OFFLINE: 'E5002',
  
  // 系统错误 (9xxx)
  INTERNAL_ERROR: 'E9001',
  SERVICE_UNAVAILABLE: 'E9002',
  RATE_LIMIT_EXCEEDED: 'E9003',
} as const;

export type ErrorCode = typeof ErrorCodes[keyof typeof ErrorCodes];

// ============================================================================
// 协议版本常量
// ============================================================================

export const PROTOCOL_VERSION = '1.0.0';

// ============================================================================
// 导出所有类型
// ============================================================================

export type {
  // 消息类型
  ClientMessage,
  ServerMessage,
  
  // Payload 类型
  PingPayload,
  AuthPayload,
  JoinRoomPayload,
  LeaveRoomPayload,
  CreateRoomPayload,
  SendMessagePayload,
  GetRoomMembersPayload,
  GetUserRoomsPayload,
  PongPayload,
  AuthSuccessPayload,
  AuthFailurePayload,
  RoomJoinedPayload,
  RoomLeftPayload,
  RoomCreatedPayload,
  UserJoinedPayload,
  UserLeftPayload,
  MessagePayload,
  BroadcastPayload,
  RoomMembersPayload,
  UserRoomsPayload,
  ErrorPayload,
  NotificationPayload,
  
  // 连接管理类型
  ConnectionMetadata,
  
  // 房间管理类型
  RoomMetadata,
  RoomMember,
  Room,
  
  // 服务端类型
  AuthHandler,
  AuthResult,
  ServerConfig,
  ServerStatus,
  
  // 客户端类型
  ClientOptions,
  ClientStatus,
  
  // 函数类型
  MessageHandler,
  EventListener,
  ClientEventHandler,
  
  // 基础类型
  Logger,
};

export {
  // 枚举
  ClientMessageType,
  ServerMessageType,
  SystemEventType,
  ErrorCodes,
  PROTOCOL_VERSION,
};
