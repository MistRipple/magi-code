/**
 * WebSocket 消息协议类型定义
 * 
 * @module Protocol
 * @description 定义消息格式、类型枚举、错误码等核心协议规范
 * @author Architecture Team
 * @version 1.0.0
 */

/**
 * 消息类型枚举
 * {{ AURA: Add - 定义系统和业务消息类型 }}
 */
export enum MessageType {
  // ========== 系统消息 ==========
  /** 系统消息：连接建立成功 */
  CONNECT = 'CONNECT',
  /** 系统消息：连接断开 */
  DISCONNECT = 'DISCONNECT',
  /** 系统消息：心跳检测请求 */
  HEARTBEAT = 'HEARTBEAT',
  /** 系统消息：心跳响应 */
  HEARTBEAT_ACK = 'HEARTBEAT_ACK',
  /** 系统消息：认证请求 */
  AUTH = 'AUTH',
  /** 系统消息：认证响应 */
  AUTH_ACK = 'AUTH_ACK',
  
  // ========== 业务消息 ==========
  /** 业务消息：单播消息（点对点） */
  MESSAGE = 'MESSAGE',
  /** 业务消息：广播消息（一对多） */
  BROADCAST = 'BROADCAST',
  /** 业务消息：订阅频道 */
  SUBSCRIBE = 'SUBSCRIBE',
  /** 业务消息：取消订阅 */
  UNSUBSCRIBE = 'UNSUBSCRIBE',
  /** 业务消息：加入房间 */
  JOIN_ROOM = 'JOIN_ROOM',
  /** 业务消息：离开房间 */
  LEAVE_ROOM = 'LEAVE_ROOM',
  
  // ========== 响应消息 ==========
  /** 响应消息：操作成功确认 */
  ACK = 'ACK',
  /** 响应消息：错误响应 */
  ERROR = 'ERROR',
}

/**
 * 错误码枚举
 * {{ AURA: Add - 定义分类错误码体系 }}
 */
export enum ErrorCode {
  // ========== 1xxx: 消息格式错误 ==========
  /** 未知错误 */
  UNKNOWN_ERROR = 1000,
  /** 无效的消息格式 */
  INVALID_MESSAGE_FORMAT = 1001,
  /** 无效的消息类型 */
  INVALID_MESSAGE_TYPE = 1002,
  /** 消息体过大 */
  MESSAGE_TOO_LARGE = 1003,
  /** 缺少必需字段 */
  MISSING_REQUIRED_FIELD = 1004,
  /** JSON 解析失败 */
  JSON_PARSE_ERROR = 1005,
  
  // ========== 2xxx: 认证和权限错误 ==========
  /** 认证失败 */
  AUTH_FAILED = 2001,
  /** 未认证 */
  UNAUTHORIZED = 2002,
  /** 权限不足 */
  FORBIDDEN = 2003,
  /** 会话过期 */
  SESSION_EXPIRED = 2004,
  /** Token 无效 */
  INVALID_TOKEN = 2005,
  
  // ========== 3xxx: 连接相关错误 ==========
  /** 连接已关闭 */
  CONNECTION_CLOSED = 3001,
  /** 连接超时 */
  CONNECTION_TIMEOUT = 3002,
  /** 心跳超时 */
  HEARTBEAT_TIMEOUT = 3003,
  /** 连接池已满 */
  CONNECTION_POOL_FULL = 3004,
  /** 连接未就绪 */
  CONNECTION_NOT_READY = 3005,
  
  // ========== 4xxx: 业务逻辑错误 ==========
  /** 频道不存在 */
  CHANNEL_NOT_FOUND = 4001,
  /** 订阅失败 */
  SUBSCRIBE_FAILED = 4002,
  /** 目标客户端不存在 */
  TARGET_NOT_FOUND = 4003,
  /** 房间不存在 */
  ROOM_NOT_FOUND = 4004,
  /** 房间已满 */
  ROOM_FULL = 4005,
  /** 操作不被允许 */
  OPERATION_NOT_ALLOWED = 4006,
  
  // ========== 5xxx: 服务器错误 ==========
  /** 服务器内部错误 */
  INTERNAL_ERROR = 5000,
  /** 服务器过载 */
  SERVER_OVERLOAD = 5001,
  /** 服务器维护中 */
  SERVER_MAINTENANCE = 5002,
}

/**
 * 连接状态枚举
 * {{ AURA: Add - 定义连接生命周期状态 }}
 */
export enum ConnectionState {
  /** 正在连接 */
  CONNECTING = 'CONNECTING',
  /** 已连接 */
  CONNECTED = 'CONNECTED',
  /** 正在断开 */
  DISCONNECTING = 'DISCONNECTING',
  /** 已断开 */
  DISCONNECTED = 'DISCONNECTED',
  /** 重连中 */
  RECONNECTING = 'RECONNECTING',
  /** 连接失败 */
  FAILED = 'FAILED',
}

/**
 * 消息头部信息
 * {{ AURA: Add - 定义消息元数据结构 }}
 */
export interface MessageHeader {
  /** 消息唯一ID，用于追踪、去重和确认 */
  id: string;
  /** 消息类型 */
  type: MessageType;
  /** 时间戳（毫秒），消息创建时间 */
  timestamp: number;
  /** 消息版本号，用于协议升级兼容 */
  version?: string;
  /** 发送者ID（客户端ID或服务端标识） */
  from?: string;
  /** 目标接收者ID（可选，单播时使用） */
  to?: string;
  /** 关联的消息ID（用于响应、确认等场景） */
  correlationId?: string;
}

/**
 * 消息负载数据
 * {{ AURA: Add - 定义灵活的消息体结构 }}
 */
export interface MessagePayload<T = any> {
  /** 业务数据，类型由具体业务定义 */
  data?: T;
  /** 目标客户端ID（单播时使用） */
  targetId?: string;
  /** 频道名称（订阅/广播时使用） */
  channel?: string;
  /** 房间ID（房间相关操作时使用） */
  roomId?: string;
  /** 元数据（可选的附加信息，如优先级、标签等） */
  metadata?: Record<string, any>;
}

/**
 * 错误消息负载
 * {{ AURA: Add - 定义统一的错误响应格式 }}
 */
export interface ErrorPayload {
  /** 错误码 */
  code: ErrorCode;
  /** 错误消息（人类可读） */
  message: string;
  /** 错误详情（可选，用于调试） */
  details?: any;
  /** 关联的消息ID（如果错误是针对某个消息的） */
  relatedMessageId?: string;
  /** 堆栈信息（仅开发环境） */
  stack?: string;
}

/**
 * 确认消息负载
 * {{ AURA: Add - 定义操作确认响应格式 }}
 */
export interface AckPayload {
  /** 确认的消息ID */
  relatedMessageId: string;
  /** 确认状态 */
  status: 'success' | 'partial' | 'failed';
  /** 附加信息（如影响的记录数、返回值等） */
  info?: any;
}

/**
 * 完整的 WebSocket 消息结构
 * {{ AURA: Add - 定义顶层消息封装格式 }}
 */
export interface WSMessage<T = any> {
  /** 消息头 */
  header: MessageHeader;
  /** 消息负载（联合类型，根据 header.type 判断） */
  payload: MessagePayload<T> | ErrorPayload | AckPayload;
}

// ========== 特定消息类型的数据结构 ==========

/**
 * 心跳消息数据
 * {{ AURA: Add - 定义心跳协议数据 }}
 */
export interface HeartbeatData {
  /** 客户端当前时间戳 */
  clientTimestamp: number;
  /** 客户端序列号（用于检测消息丢失） */
  sequence?: number;
  /** 客户端状态信息（可选） */
  status?: {
    /** 缓冲区队列长度 */
    queueSize?: number;
    /** 网络延迟（毫秒） */
    latency?: number;
  };
}

/**
 * 心跳响应数据
 * {{ AURA: Add - 定义心跳响应数据 }}
 */
export interface HeartbeatAckData {
  /** 服务端时间戳 */
  serverTimestamp: number;
  /** 客户端发送的时间戳（回传用于计算延迟） */
  clientTimestamp: number;
  /** 服务端负载信息（可选） */
  serverLoad?: {
    /** 当前连接数 */
    connections?: number;
    /** CPU 使用率 */
    cpu?: number;
  };
}

/**
 * 连接建立消息数据
 * {{ AURA: Add - 定义连接握手数据 }}
 */
export interface ConnectData {
  /** 客户端ID（可选，服务端会自动分配） */
  clientId?: string;
  /** 认证令牌（可选，如果需要认证） */
  token?: string;
  /** 客户端信息 */
  clientInfo?: {
    /** 用户代理字符串 */
    userAgent?: string;
    /** 平台信息（web/mobile/desktop） */
    platform?: string;
    /** 客户端版本号 */
    version?: string;
    /** 设备信息 */
    deviceId?: string;
  };
  /** 连接选项 */
  options?: {
    /** 是否启用压缩 */
    compression?: boolean;
    /** 心跳间隔（毫秒） */
    heartbeatInterval?: number;
  };
}

/**
 * 连接成功响应数据
 * {{ AURA: Add - 定义连接成功返回信息 }}
 */
export interface ConnectAckData {
  /** 服务端分配的连接ID */
  connectionId: string;
  /** 会话ID（可选） */
  sessionId?: string;
  /** 服务端配置 */
  serverConfig?: {
    /** 心跳间隔（毫秒） */
    heartbeatInterval: number;
    /** 心跳超时（毫秒） */
    heartbeatTimeout: number;
    /** 最大消息大小（字节） */
    maxMessageSize: number;
  };
  /** 重连令牌（用于断线重连） */
  reconnectToken?: string;
}

/**
 * 订阅消息数据
 * {{ AURA: Add - 定义频道订阅数据 }}
 */
export interface SubscribeData {
  /** 订阅的频道列表 */
  channels: string[];
  /** 订阅选项 */
  options?: {
    /** 是否接收历史消息 */
    includeHistory?: boolean;
    /** 历史消息数量限制 */
    historyLimit?: number;
    /** 消息过滤器（可选） */
    filter?: Record<string, any>;
  };
}

/**
 * 房间操作数据
 * {{ AURA: Add - 定义房间管理数据 }}
 */
export interface RoomData {
  /** 房间ID */
  roomId: string;
  /** 房间名称 */
  roomName?: string;
  /** 房间元数据 */
  metadata?: Record<string, any>;
}

// ========== 协议常量 ==========

/**
 * 协议版本
 * {{ AURA: Add - 定义当前协议版本 }}
 */
export const PROTOCOL_VERSION = '1.0.0';

/**
 * 消息大小限制（字节）
 * {{ AURA: Add - 定义消息大小上限 }}
 */
export const MAX_MESSAGE_SIZE = 1024 * 1024; // 1MB

/**
 * 默认心跳配置
 * {{ AURA: Add - 定义默认心跳参数 }}
 */
export const DEFAULT_HEARTBEAT_CONFIG = {
  /** 客户端心跳间隔（毫秒） */
  CLIENT_INTERVAL: 25000, // 25秒
  /** 服务端心跳检测间隔（毫秒） */
  SERVER_INTERVAL: 30000, // 30秒
  /** 心跳超时时间（毫秒） */
  TIMEOUT: 35000, // 35秒
} as const;

/**
 * 重连配置
 * {{ AURA: Add - 定义重连策略参数 }}
 */
export const DEFAULT_RECONNECT_CONFIG = {
  /** 最大重连次数 */
  MAX_ATTEMPTS: 5,
  /** 初始延迟（毫秒） */
  INITIAL_DELAY: 1000, // 1秒
  /** 最大延迟（毫秒） */
  MAX_DELAY: 30000, // 30秒
  /** 延迟增长因子（指数退避） */
  BACKOFF_FACTOR: 1.5,
} as const;

// ========== 类型守卫函数 ==========

/**
 * 检查是否为错误负载
 * {{ AURA: Add - 提供类型守卫辅助函数 }}
 */
export function isErrorPayload(payload: any): payload is ErrorPayload {
  return payload && typeof payload.code === 'number' && typeof payload.message === 'string';
}

/**
 * 检查是否为确认负载
 * {{ AURA: Add - 提供类型守卫辅助函数 }}
 */
export function isAckPayload(payload: any): payload is AckPayload {
  return payload && typeof payload.relatedMessageId === 'string' && typeof payload.status === 'string';
}

/**
 * 检查是否为消息负载
 * {{ AURA: Add - 提供类型守卫辅助函数 }}
 */
export function isMessagePayload(payload: any): payload is MessagePayload {
  return payload && !isErrorPayload(payload) && !isAckPayload(payload);
}
