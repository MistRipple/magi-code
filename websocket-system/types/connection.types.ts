/**
 * 连接管理相关类型定义
 * 
 * @module Connection
 * @description 定义连接池、连接状态、连接元数据等相关类型
 * @author Architecture Team
 * @version 1.0.0
 */

import { ConnectionState } from './protocol.types';

/**
 * 连接元数据
 * {{ AURA: Add - 定义连接的附加信息 }}
 */
export interface ConnectionMetadata {
  /** 客户端 IP 地址 */
  ip?: string;
  /** 用户代理字符串 */
  userAgent?: string;
  /** 平台信息 */
  platform?: string;
  /** 设备ID */
  deviceId?: string;
  /** 用户ID（业务层标识） */
  userId?: string;
  /** 自定义标签 */
  tags?: string[];
  /** 额外的元数据 */
  extra?: Record<string, any>;
}

/**
 * 连接信息
 * {{ AURA: Add - 定义完整的连接对象结构 }}
 */
export interface ConnectionInfo {
  /** 连接唯一ID */
  id: string;
  /** WebSocket 连接对象（服务端） */
  ws?: any; // WebSocket from 'ws' library
  /** 连接状态 */
  state: ConnectionState;
  /** 连接建立时间（时间戳） */
  connectedAt: number;
  /** 上次活动时间（时间戳） */
  lastActiveAt: number;
  /** 上次心跳时间（时间戳） */
  lastHeartbeat: number;
  /** 连接元数据 */
  metadata: ConnectionMetadata;
  /** 已订阅的频道列表 */
  subscribedChannels: Set<string>;
  /** 已加入的房间列表 */
  joinedRooms: Set<string>;
  /** 消息发送计数 */
  messagesSent: number;
  /** 消息接收计数 */
  messagesReceived: number;
  /** 是否已认证 */
  authenticated: boolean;
  /** 认证信息（如果已认证） */
  authInfo?: {
    userId?: string;
    roles?: string[];
    permissions?: string[];
  };
}

/**
 * 连接池统计信息
 * {{ AURA: Add - 定义连接池监控数据 }}
 */
export interface ConnectionPoolStats {
  /** 总连接数 */
  totalConnections: number;
  /** 活跃连接数 */
  activeConnections: number;
  /** 空闲连接数 */
  idleConnections: number;
  /** 重连中的连接数 */
  reconnectingConnections: number;
  /** 平均连接时长（毫秒） */
  averageConnectionDuration: number;
  /** 最大并发连接数 */
  peakConnections: number;
  /** 总发送消息数 */
  totalMessagesSent: number;
  /** 总接收消息数 */
  totalMessagesReceived: number;
}

/**
 * 连接配置选项
 * {{ AURA: Add - 定义连接管理器配置 }}
 */
export interface ConnectionManagerOptions {
  /** 最大连接数 */
  maxConnections?: number;
  /** 连接超时时间（毫秒） */
  connectionTimeout?: number;
  /** 心跳间隔（毫秒） */
  heartbeatInterval?: number;
  /** 心跳超时（毫秒） */
  heartbeatTimeout?: number;
  /** 是否启用自动清理 */
  enableAutoCleaning?: boolean;
  /** 自动清理间隔（毫秒） */
  cleaningInterval?: number;
  /** 空闲超时时间（毫秒，超过此时间未活动则清理） */
  idleTimeout?: number;
}

/**
 * 连接事件类型
 * {{ AURA: Add - 定义连接生命周期事件 }}
 */
export enum ConnectionEvent {
  /** 连接建立 */
  CONNECTED = 'connected',
  /** 连接断开 */
  DISCONNECTED = 'disconnected',
  /** 连接错误 */
  ERROR = 'error',
  /** 心跳超时 */
  HEARTBEAT_TIMEOUT = 'heartbeat_timeout',
  /** 连接空闲 */
  IDLE = 'idle',
  /** 连接认证 */
  AUTHENTICATED = 'authenticated',
  /** 状态变更 */
  STATE_CHANGED = 'state_changed',
}

/**
 * 连接事件处理器类型
 * {{ AURA: Add - 定义事件回调函数签名 }}
 */
export type ConnectionEventHandler = (connectionId: string, data?: any) => void | Promise<void>;

/**
 * 连接过滤器
 * {{ AURA: Add - 定义连接查询过滤条件 }}
 */
export interface ConnectionFilter {
  /** 连接状态过滤 */
  state?: ConnectionState | ConnectionState[];
  /** 用户ID过滤 */
  userId?: string;
  /** 是否已认证 */
  authenticated?: boolean;
  /** 订阅的频道（包含任一） */
  channel?: string;
  /** 加入的房间（包含任一） */
  roomId?: string;
  /** 标签过滤（包含任一） */
  tags?: string[];
  /** 自定义过滤函数 */
  custom?: (conn: ConnectionInfo) => boolean;
}

/**
 * 连接操作结果
 * {{ AURA: Add - 定义操作返回结果 }}
 */
export interface ConnectionOperationResult {
  /** 操作是否成功 */
  success: boolean;
  /** 受影响的连接ID */
  connectionId?: string;
  /** 错误信息（如果失败） */
  error?: string;
  /** 额外数据 */
  data?: any;
}

/**
 * 批量连接操作结果
 * {{ AURA: Add - 定义批量操作返回结果 }}
 */
export interface BatchConnectionOperationResult {
  /** 成功数量 */
  successCount: number;
  /** 失败数量 */
  failedCount: number;
  /** 成功的连接ID列表 */
  successIds: string[];
  /** 失败的连接ID列表 */
  failedIds: string[];
  /** 详细错误信息 */
  errors?: Array<{ connectionId: string; error: string }>;
}
