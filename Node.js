/**
 * WebSocket 服务端 - 契约集成实现
 * 契约ID: contract_1770008190053_06v7ehbw0
 * 版本: v1.0.0
 * 
 * 本文件遵循 WebSocket 实时消息推送系统接口契约，提供完整的服务端实现
 */

import { WebSocketServer as WSServer } from 'ws';
import { createServer } from 'http';
import { EventEmitter } from 'events';

// ==================== 类型定义（契约要求） ====================

/**
 * 服务器配置
 * @interface ServerConfig
 */
const DEFAULT_SERVER_CONFIG = {
  port: 8080,
  host: '0.0.0.0',
  heartbeatInterval: 30000,      // 30秒
  heartbeatTimeout: 60000,        // 60秒
  maxConnections: 10000,
  enableCompression: false,
  maxPayloadLength: 1024 * 1024,  // 1MB
  logLevel: 'info'
};

/**
 * 系统事件类型（契约定义）
 */
const SystemEventType = {
  // 连接事件
  CONNECTION_OPENED: 'connection:opened',
  CONNECTION_CLOSED: 'connection:closed',
  CONNECTION_ERROR: 'connection:error',
  HEARTBEAT_TIMEOUT: 'heartbeat:timeout',
  
  // 消息事件
  MESSAGE_RECEIVED: 'message:received',
  MESSAGE_SENT: 'message:sent',
  MESSAGE_ERROR: 'message:error',
  
  // 房间事件
  ROOM_CREATED: 'room:created',
  ROOM_DELETED: 'room:deleted',
  USER_JOINED_ROOM: 'user:joined_room',
  USER_LEFT_ROOM: 'user:left_room',
  
  // 系统事件
  SERVER_STARTED: 'server:started',
  SERVER_STOPPED: 'server:stopped',
  SERVER_ERROR: 'server:error',
};

// ==================== EventDispatcher (契约要求) ====================

/**
 * 事件分发器
 * 契约要求：支持异步监听器，错误隔离，按注册顺序执行
 */
class EventDispatcher {
  constructor() {
    this.emitter = new EventEmitter();
    this.emitter.setMaxListeners(100); // 避免内存泄漏警告
  }

  /**
   * 注册事件监听器
   * @param {string} eventType - 事件类型
   * @param {Function} listener - 监听器函数
   */
  on(eventType, listener) {
    this.emitter.on(eventType, listener);
  }

  /**
   * 注册一次性事件监听器
   * @param {string} eventType - 事件类型
   * @param {Function} listener - 监听器函数
   */
  once(eventType, listener) {
    this.emitter.once(eventType, listener);
  }

  /**
   * 注销事件监听器
   * @param {string} eventType - 事件类型
   * @param {Function} listener - 监听器函数（可选）
   */
  off(eventType, listener) {
    if (listener) {
      this.emitter.off(eventType, listener);
    } else {
      this.emitter.removeAllListeners(eventType);
    }
  }

  /**
   * 触发事件（支持异步监听器，错误隔离）
   * @param {string} eventType - 事件类型
   * @param {any} data - 事件数据
   */
  emit(eventType, data) {
    const listeners = this.emitter.listeners(eventType);
    
    listeners.forEach(async (listener) => {
      try {
        await listener(data);
      } catch (error) {
        console.error(`[EventDispatcher] 监听器错误 (${eventType}):`, error);
      }
    });
  }

  /**
   * 获取事件监听器数量
   * @param {string} eventType - 事件类型
   * @returns {number}
   */
  listenerCount(eventType) {
    return this.emitter.listenerCount(eventType);
  }
}

// ==================== ConnectionManager (契约要求) ====================

/**
 * 连接管理器
 * 契约要求：线程安全、自动清理、事件触发、内存管理
 */
class ConnectionManager {
  constructor(eventDispatcher) {
    this.eventDispatcher = eventDispatcher;
    
    // 连接存储：Map<connectionId, { ws, metadata }>
    this.connections = new Map();
    
    // 用户连接映射：Map<userId, Set<connectionId>>
    this.userConnections = new Map();
    
    // 心跳定时器
    this.heartbeatTimer = null;
  }

  /**
   * 注册新连接
   * @param {string} connectionId - 连接ID（必须唯一）
   * @param {WebSocket} ws - WebSocket 实例
   * @param {string} userId - 用户ID（可选）
   * @throws {Error} 连接ID已存在
   */
  registerConnection(connectionId, ws, userId = null) {
    if (this.connections.has(connectionId)) {
      throw new Error(`连接ID已存在: ${connectionId}`);
    }

    const metadata = {
      connectionId,
      userId,
      connectedAt: Date.now(),
      lastHeartbeat: Date.now(),
      isAlive: true,
      userAgent: null,
      ipAddress: null,
      customData: {}
    };

    this.connections.set(connectionId, { ws, metadata });

    // 如果有用户ID，建立映射
    if (userId) {
      if (!this.userConnections.has(userId)) {
        this.userConnections.set(userId, new Set());
      }
      this.userConnections.get(userId).add(connectionId);
    }

    // 触发事件
    this.eventDispatcher.emit(SystemEventType.CONNECTION_OPENED, { connectionId, userId });
  }

  /**
   * 注销连接
   * @param {string} connectionId - 连接ID
   * @returns {boolean} 是否成功注销
   */
  unregisterConnection(connectionId) {
    const conn = this.connections.get(connectionId);
    if (!conn) {
      return false;
    }

    const { metadata } = conn;

    // 清理用户连接映射
    if (metadata.userId) {
      const userConns = this.userConnections.get(metadata.userId);
      if (userConns) {
        userConns.delete(connectionId);
        if (userConns.size === 0) {
          this.userConnections.delete(metadata.userId);
        }
      }
    }

    // 删除连接
    this.connections.delete(connectionId);

    // 触发事件
    this.eventDispatcher.emit(SystemEventType.CONNECTION_CLOSED, {
      connectionId,
      userId: metadata.userId
    });

    return true;
  }

  /**
   * 获取连接实例
   * @param {string} connectionId - 连接ID
   * @returns {WebSocket | undefined}
   */
  getConnection(connectionId) {
    const conn = this.connections.get(connectionId);
    return conn ? conn.ws : undefined;
  }

  /**
   * 根据用户ID获取所有连接ID
   * @param {string} userId - 用户ID
   * @returns {string[]} 连接ID列表
   */
  getConnectionsByUserId(userId) {
    const connSet = this.userConnections.get(userId);
    return connSet ? Array.from(connSet) : [];
  }

  /**
   * 获取连接元数据
   * @param {string} connectionId - 连接ID
   * @returns {Object | undefined}
   */
  getConnectionMetadata(connectionId) {
    const conn = this.connections.get(connectionId);
    return conn ? conn.metadata : undefined;
  }

  /**
   * 更新连接元数据
   * @param {string} connectionId - 连接ID
   * @param {Object} metadata - 部分元数据
   * @returns {boolean} 是否成功更新
   */
  updateConnectionMetadata(connectionId, metadata) {
    const conn = this.connections.get(connectionId);
    if (!conn) {
      return false;
    }

    conn.metadata = { ...conn.metadata, ...metadata };
    return true;
  }

  /**
   * 获取所有活跃连接
   * @returns {Map<string, WebSocket>}
   */
  getAllConnections() {
    const result = new Map();
    this.connections.forEach((conn, id) => {
      result.set(id, conn.ws);
    });
    return result;
  }

  /**
   * 启动心跳检测
   * @param {number} interval - 检测间隔（毫秒）
   */
  startHeartbeat(interval = 30000) {
    if (this.heartbeatTimer) {
      return;
    }

    this.heartbeatTimer = setInterval(() => {
      const now = Date.now();
      const timeout = interval * 2; // 超时时间为间隔的2倍

      this.connections.forEach((conn, connectionId) => {
        const { ws, metadata } = conn;

        // 检查心跳超时
        if (now - metadata.lastHeartbeat > timeout) {
          console.warn(`[ConnectionManager] 心跳超时: ${connectionId}`);
          
          // 触发超时事件
          this.eventDispatcher.emit(SystemEventType.HEARTBEAT_TIMEOUT, {
            connectionId,
            userId: metadata.userId
          });

          // 关闭连接
          ws.terminate();
          this.unregisterConnection(connectionId);
          return;
        }

        // 发送 PING
        if (ws.readyState === 1) {
          metadata.isAlive = false;
          ws.ping();
        }
      });
    }, interval);

    console.log(`[ConnectionManager] 心跳检测已启动，间隔: ${interval}ms`);
  }

  /**
   * 停止心跳检测
   */
  stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
      console.log('[ConnectionManager] 心跳检测已停止');
    }
  }

  /**
   * 检查连接是否存活
   * @param {string} connectionId - 连接ID
   * @returns {boolean}
   */
  isAlive(connectionId) {
    const conn = this.connections.get(connectionId);
    return conn ? conn.metadata.isAlive : false;
  }

  /**
   * 处理 PING 消息（更新最后心跳时间）
   * @param {string} connectionId - 连接ID
   */
  handlePing(connectionId) {
    const conn = this.connections.get(connectionId);
    if (conn) {
      conn.metadata.lastHeartbeat = Date.now();
      conn.metadata.isAlive = true;
    }
  }

  /**
   * 获取统计信息
   * @returns {Object}
   */
  getStats() {
    return {
      total: this.connections.size,
      totalUsers: this.userConnections.size
    };
  }
}

// ==================== RoomManager (契约要求) ====================

/**
 * 房间管理器
 * 契约要求：容量检查、自动清理、事件触发、多端支持
 */
class RoomManager {
  constructor(connectionManager, eventDispatcher) {
    this.connectionManager = connectionManager;
    this.eventDispatcher = eventDispatcher;
    
    // 房间存储：Map<roomId, Room>
    this.rooms = new Map();
    
    // 用户房间映射：Map<userId, Set<roomId>>
    this.userRooms = new Map();
  }

  /**
   * 创建房间
   * @param {string} roomId - 房间ID
   * @param {Object} metadata - 房间元数据（可选）
   * @throws {Error} 房间已存在
   */
  createRoom(roomId, metadata = {}) {
    if (this.rooms.has(roomId)) {
      throw new Error(`房间已存在: ${roomId}`);
    }

    const room = {
      metadata: {
        roomId,
        name: metadata.name || roomId,
        description: metadata.description || '',
        createdAt: Date.now(),
        createdBy: metadata.createdBy,
        maxMembers: metadata.maxMembers || 100,
        isPrivate: metadata.isPrivate || false,
        customData: metadata.customData || {}
      },
      members: new Map()
    };

    this.rooms.set(roomId, room);

    // 触发事件
    this.eventDispatcher.emit(SystemEventType.ROOM_CREATED, {
      roomId,
      metadata: room.metadata
    });

    console.log(`[RoomManager] 房间已创建: ${roomId}`);
  }

  /**
   * 删除房间
   * @param {string} roomId - 房间ID
   * @returns {boolean} 是否成功删除
   */
  deleteRoom(roomId) {
    const room = this.rooms.get(roomId);
    if (!room) {
      return false;
    }

    // 清理用户房间映射
    room.members.forEach((member, userId) => {
      const userRoomSet = this.userRooms.get(userId);
      if (userRoomSet) {
        userRoomSet.delete(roomId);
        if (userRoomSet.size === 0) {
          this.userRooms.delete(userId);
        }
      }
    });

    this.rooms.delete(roomId);

    // 触发事件
    this.eventDispatcher.emit(SystemEventType.ROOM_DELETED, { roomId });

    console.log(`[RoomManager] 房间已删除: ${roomId}`);
    return true;
  }

  /**
   * 用户加入房间
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @param {string} connectionId - 连接ID
   * @returns {boolean} 是否成功加入
   * @throws {Error} 房间不存在或已满
   */
  joinRoom(roomId, userId, connectionId) {
    const room = this.rooms.get(roomId);
    if (!room) {
      throw new Error(`房间不存在: ${roomId}`);
    }

    // 检查容量限制
    if (room.members.size >= room.metadata.maxMembers && !room.members.has(userId)) {
      throw new Error(`房间已满: ${roomId}`);
    }

    // 添加或更新成员
    if (room.members.has(userId)) {
      const member = room.members.get(userId);
      if (!member.connectionIds.includes(connectionId)) {
        member.connectionIds.push(connectionId);
      }
    } else {
      room.members.set(userId, {
        userId,
        connectionIds: [connectionId],
        joinedAt: Date.now(),
        role: room.members.size === 0 ? 'owner' : 'member'
      });
    }

    // 更新用户房间映射
    if (!this.userRooms.has(userId)) {
      this.userRooms.set(userId, new Set());
    }
    this.userRooms.get(userId).add(roomId);

    // 触发事件
    this.eventDispatcher.emit(SystemEventType.USER_JOINED_ROOM, {
      roomId,
      userId,
      connectionId
    });

    console.log(`[RoomManager] 用户 ${userId} 加入房间 ${roomId}`);
    return true;
  }

  /**
   * 用户离开房间
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @param {string} connectionId - 连接ID（可选）
   * @returns {boolean} 是否成功离开
   */
  leaveRoom(roomId, userId, connectionId = null) {
    const room = this.rooms.get(roomId);
    if (!room || !room.members.has(userId)) {
      return false;
    }

    const member = room.members.get(userId);

    if (connectionId) {
      // 只移除特定连接
      member.connectionIds = member.connectionIds.filter(id => id !== connectionId);
      
      if (member.connectionIds.length === 0) {
        room.members.delete(userId);
        
        const userRoomSet = this.userRooms.get(userId);
        if (userRoomSet) {
          userRoomSet.delete(roomId);
          if (userRoomSet.size === 0) {
            this.userRooms.delete(userId);
          }
        }
      }
    } else {
      // 移除所有连接
      room.members.delete(userId);
      
      const userRoomSet = this.userRooms.get(userId);
      if (userRoomSet) {
        userRoomSet.delete(roomId);
        if (userRoomSet.size === 0) {
          this.userRooms.delete(userId);
        }
      }
    }

    // 触发事件
    this.eventDispatcher.emit(SystemEventType.USER_LEFT_ROOM, {
      roomId,
      userId,
      connectionId
    });

    console.log(`[RoomManager] 用户 ${userId} 离开房间 ${roomId}`);

    // 自动清理空房间
    if (room.members.size === 0) {
      this.deleteRoom(roomId);
    }

    return true;
  }

  /**
   * 获取房间成员列表
   * @param {string} roomId - 房间ID
   * @returns {Array} 成员列表
   */
  getRoomMembers(roomId) {
    const room = this.rooms.get(roomId);
    if (!room) {
      return [];
    }

    return Array.from(room.members.values());
  }

  /**
   * 获取房间元数据
   * @param {string} roomId - 房间ID
   * @returns {Object | undefined}
   */
  getRoomMetadata(roomId) {
    const room = this.rooms.get(roomId);
    return room ? room.metadata : undefined;
  }

  /**
   * 更新房间元数据
   * @param {string} roomId - 房间ID
   * @param {Object} metadata - 部分元数据
   * @returns {boolean} 是否成功更新
   */
  updateRoomMetadata(roomId, metadata) {
    const room = this.rooms.get(roomId);
    if (room) {
      room.metadata = { ...room.metadata, ...metadata };
      return true;
    }
    return false;
  }

  /**
   * 获取用户加入的所有房间
   * @param {string} userId - 用户ID
   * @returns {string[]} 房间ID列表
   */
  getUserRooms(userId) {
    const roomSet = this.userRooms.get(userId);
    return roomSet ? Array.from(roomSet) : [];
  }

  /**
   * 检查房间是否存在
   * @param {string} roomId - 房间ID
   * @returns {boolean}
   */
  roomExists(roomId) {
    return this.rooms.has(roomId);
  }

  /**
   * 检查用户是否在房间中
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @returns {boolean}
   */
  isUserInRoom(roomId, userId) {
    const room = this.rooms.get(roomId);
    return room ? room.members.has(userId) : false;
  }

  /**
   * 获取所有房间
   * @returns {Map<string, Room>}
   */
  getAllRooms() {
    return new Map(this.rooms);
  }

  /**
   * 获取统计信息
   * @returns {Object}
   */
  getStats() {
    return {
      totalRooms: this.rooms.size,
      totalUsersInRooms: this.userRooms.size
    };
  }
}

// ==================== MessageRouter (契约要求) ====================

/**
 * 消息路由器
 * 契约要求：消息验证、错误隔离、处理器注册、性能优化
 */
class MessageRouter {
  constructor(connectionManager, roomManager, eventDispatcher) {
    this.connectionManager = connectionManager;
    this.roomManager = roomManager;
    this.eventDispatcher = eventDispatcher;
    
    // 消息处理器：Map<messageType, handler>
    this.handlers = new Map();
    
    // 注册默认处理器
    this.registerDefaultHandlers();
  }

  /**
   * 注册默认消息处理器
   */
  registerDefaultHandlers() {
    // PING-PONG 心跳
    this.registerHandler('ping', async (message, connectionId) => {
      this.connectionManager.handlePing(connectionId);
      await this.sendToConnection(connectionId, {
        type: 'pong',
        timestamp: Date.now(),
        replyTo: message.id
      });
    });

    // 加入房间
    this.registerHandler('join_room', async (message, connectionId) => {
      const { roomId } = message.payload || {};
      const metadata = this.connectionManager.getConnectionMetadata(connectionId);
      const userId = metadata?.userId || connectionId;

      try {
        // 房间不存在则自动创建
        if (!this.roomManager.roomExists(roomId)) {
          this.roomManager.createRoom(roomId);
        }

        this.roomManager.joinRoom(roomId, userId, connectionId);

        await this.sendToConnection(connectionId, {
          type: 'room_joined',
          payload: {
            roomId,
            members: this.roomManager.getRoomMembers(roomId)
          },
          replyTo: message.id,
          timestamp: Date.now()
        });
      } catch (error) {
        await this.sendToConnection(connectionId, {
          type: 'error',
          payload: { code: 'JOIN_FAILED', message: error.message },
          replyTo: message.id,
          timestamp: Date.now()
        });
      }
    });

    // 离开房间
    this.registerHandler('leave_room', async (message, connectionId) => {
      const { roomId } = message.payload || {};
      const metadata = this.connectionManager.getConnectionMetadata(connectionId);
      const userId = metadata?.userId || connectionId;

      this.roomManager.leaveRoom(roomId, userId, connectionId);

      await this.sendToConnection(connectionId, {
        type: 'room_left',
        payload: { roomId },
        replyTo: message.id,
        timestamp: Date.now()
      });
    });

    // 发送消息
    this.registerHandler('send_message', async (message, connectionId) => {
      const { targetType, targetId, content } = message.payload || {};
      const metadata = this.connectionManager.getConnectionMetadata(connectionId);
      const userId = metadata?.userId || connectionId;

      if (targetType === 'user') {
        await this.sendToUser(targetId, {
          type: 'message',
          payload: { fromUserId: userId, content },
          timestamp: Date.now()
        });
      } else if (targetType === 'room') {
        await this.sendToRoom(targetId, {
          type: 'message',
          payload: { fromUserId: userId, roomId: targetId, content },
          timestamp: Date.now()
        }, [connectionId]);
      }

      await this.sendToConnection(connectionId, {
        type: 'message_sent',
        replyTo: message.id,
        timestamp: Date.now()
      });
    });
  }

  /**
   * 路由消息到对应处理器
   * @param {Object} message - 客户端消息
   * @param {string} connectionId - 来源连接ID
   */
  async route(message, connectionId) {
    // 消息验证
    if (!message || typeof message !== 'object' || !message.type) {
      await this.sendToConnection(connectionId, {
        type: 'error',
        payload: { code: 'INVALID_MESSAGE', message: '消息格式错误' },
        timestamp: Date.now()
      });
      return;
    }

    // 触发消息接收事件
    this.eventDispatcher.emit(SystemEventType.MESSAGE_RECEIVED, {
      connectionId,
      message
    });

    const handler = this.handlers.get(message.type);
    if (handler) {
      try {
        await handler(message, connectionId);
      } catch (error) {
        console.error(`[MessageRouter] 处理器错误 (${message.type}):`, error);
        await this.sendToConnection(connectionId, {
          type: 'error',
          payload: { code: 'HANDLER_ERROR', message: '处理失败' },
          replyTo: message.id,
          timestamp: Date.now()
        });
      }
    } else {
      await this.sendToConnection(connectionId, {
        type: 'error',
        payload: { code: 'UNKNOWN_TYPE', message: `未知消息类型: ${message.type}` },
        replyTo: message.id,
        timestamp: Date.now()
      });
    }
  }

  /**
   * 广播消息给所有连接
   * @param {Object} message - 服务端消息
   * @param {string[]} excludeConnectionIds - 排除的连接ID
   * @returns {Promise<number>} 成功发送的数量
   */
  async broadcast(message, excludeConnectionIds = []) {
    const connections = this.connectionManager.getAllConnections();
    let count = 0;

    for (const [connId, ws] of connections) {
      if (excludeConnectionIds.includes(connId)) {
        continue;
      }

      if (await this.sendToConnection(connId, message)) {
        count++;
      }
    }

    return count;
  }

  /**
   * 发送点对点消息
   * @param {string} userId - 目标用户ID
   * @param {Object} message - 服务端消息
   * @returns {Promise<number>} 成功发送的数量
   */
  async sendToUser(userId, message) {
    const connectionIds = this.connectionManager.getConnectionsByUserId(userId);
    let count = 0;

    for (const connId of connectionIds) {
      if (await this.sendToConnection(connId, message)) {
        count++;
      }
    }

    return count;
  }

  /**
   * 发送消息到指定连接
   * @param {string} connectionId - 连接ID
   * @param {Object} message - 服务端消息
   * @returns {Promise<boolean>} 是否成功发送
   */
  async sendToConnection(connectionId, message) {
    const ws = this.connectionManager.getConnection(connectionId);
    if (!ws || ws.readyState !== 1) {
      return false;
    }

    try {
      const data = JSON.stringify({
        id: `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
        version: '1.0',
        ...message
      });
      
      ws.send(data);
      
      // 触发发送事件
      this.eventDispatcher.emit(SystemEventType.MESSAGE_SENT, {
        connectionId,
        message
      });
      
      return true;
    } catch (error) {
      console.error(`[MessageRouter] 发送失败:`, error);
      this.eventDispatcher.emit(SystemEventType.MESSAGE_ERROR, {
        connectionId,
        error: error.message
      });
      return false;
    }
  }

  /**
   * 发送消息到房间
   * @param {string} roomId - 房间ID
   * @param {Object} message - 服务端消息
   * @param {string[]} excludeConnectionIds - 排除的连接ID
   * @returns {Promise<number>} 成功发送的数量
   */
  async sendToRoom(roomId, message, excludeConnectionIds = []) {
    const members = this.roomManager.getRoomMembers(roomId);
    let count = 0;

    for (const member of members) {
      for (const connId of member.connectionIds) {
        if (excludeConnectionIds.includes(connId)) {
          continue;
        }

        if (await this.sendToConnection(connId, message)) {
          count++;
        }
      }
    }

    return count;
  }

  /**
   * 注册消息处理器
   * @param {string} messageType - 消息类型
   * @param {Function} handler - 处理器函数
   */
  registerHandler(messageType, handler) {
    this.handlers.set(messageType, handler);
  }

  /**
   * 注销消息处理器
   * @param {string} messageType - 消息类型
   * @returns {boolean} 是否成功注销
   */
  unregisterHandler(messageType) {
    return this.handlers.delete(messageType);
  }
}

// ==================== WebSocketServer 主类 (契约要求) ====================

/**
 * WebSocket 服务器主类
 * 契约要求：单例模式、优雅关闭、错误处理、事件触发
 */
class WebSocketServer {
  constructor(config = {}) {
    // 合并配置
    this.config = { ...DEFAULT_SERVER_CONFIG, ...config };
    
    // 核心组件
    this.eventDispatcher = new EventDispatcher();
    this.connectionManager = new ConnectionManager(this.eventDispatcher);
    this.roomManager = new RoomManager(this.connectionManager, this.eventDispatcher);
    this.messageRouter = new MessageRouter(
      this.connectionManager,
      this.roomManager,
      this.eventDispatcher
    );
    
    // 服务器实例
    this.httpServer = null;
    this.wss = null;
    
    // 服务器状态
    this.status = {
      isRunning: false,
      port: null,
      host: null,
      startedAt: null,
      totalConnections: 0,
      totalRooms: 0
    };
  }

  /**
   * 启动服务器
   * @param {number} port - 端口号（可选）
   * @param {string} host - 主机地址（可选）
   * @returns {Promise<void>}
   * @throws {Error} 端口已被占用或启动失败
   */
  async start(port, host) {
    if (this.status.isRunning) {
      throw new Error('服务器已在运行中');
    }

    const finalPort = port || this.config.port;
    const finalHost = host || this.config.host;

    return new Promise((resolve, reject) => {
      try {
        // 创建 HTTP 服务器
        this.httpServer = createServer((req, res) => {
          res.writeHead(200, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify(this.getStatus(), null, 2));
        });

        // 创建 WebSocket 服务器
        this.wss = new WSServer({
          server: this.httpServer,
          perMessageDeflate: this.config.enableCompression,
          maxPayload: this.config.maxPayloadLength
        });

        // 监听连接
        this.wss.on('connection', (ws, req) => {
          this.handleConnection(ws, req);
        });

        // 监听错误
        this.wss.on('error', (error) => {
          console.error('[WebSocketServer] 服务器错误:', error);
          this.eventDispatcher.emit(SystemEventType.SERVER_ERROR, { error });
        });

        // 启动 HTTP 服务器
        this.httpServer.listen(finalPort, finalHost, () => {
          this.status.isRunning = true;
          this.status.port = finalPort;
          this.status.host = finalHost;
          this.status.startedAt = Date.now();

          // 启动心跳检测
          this.connectionManager.startHeartbeat(this.config.heartbeatInterval);

          console.log(`✅ WebSocket 服务器已启动: ws://${finalHost}:${finalPort}`);

          // 触发启动事件
          this.eventDispatcher.emit(SystemEventType.SERVER_STARTED, {
            port: finalPort,
            host: finalHost
          });

          resolve();
        });

        // 错误处理
        this.httpServer.on('error', (error) => {
          console.error('[WebSocketServer] HTTP 服务器错误:', error);
          reject(error);
        });

      } catch (error) {
        reject(error);
      }
    });
  }

  /**
   * 停止服务器
   * @returns {Promise<void>}
   */
  async stop() {
    if (!this.status.isRunning) {
      return;
    }

    console.log('🛑 正在关闭服务器...');

    // 停止心跳
    this.connectionManager.stopHeartbeat();

    // 关闭所有连接
    const connections = this.connectionManager.getAllConnections();
    for (const [connId, ws] of connections) {
      ws.close(1000, '服务器关闭');
      this.connectionManager.unregisterConnection(connId);
    }

    // 关闭 WebSocket 服务器
    if (this.wss) {
      await new Promise((resolve) => {
        this.wss.close(() => {
          console.log('✅ WebSocket 服务器已关闭');
          resolve();
        });
      });
    }

    // 关闭 HTTP 服务器
    if (this.httpServer) {
      await new Promise((resolve) => {
        this.httpServer.close(() => {
          console.log('✅ HTTP 服务器已关闭');
          resolve();
        });
      });
    }

    this.status.isRunning = false;

    // 触发停止事件
    this.eventDispatcher.emit(SystemEventType.SERVER_STOPPED, {});
  }

  /**
   * 处理新连接
   */
  handleConnection(ws, req) {
    // 检查连接数限制
    if (this.connectionManager.getStats().total >= this.config.maxConnections) {
      ws.close(1008, '服务器连接数已达上限');
      return;
    }

    // 提取用户ID（从URL参数）
    const url = new URL(req.url, `http://${req.headers.host}`);
    const userId = url.searchParams.get('userId') || null;
    const connectionId = `conn_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

    // 注册连接
    this.connectionManager.registerConnection(connectionId, ws, userId);
    this.connectionManager.updateConnectionMetadata(connectionId, {
      userAgent: req.headers['user-agent'],
      ipAddress: req.socket.remoteAddress
    });

    // 发送连接成功消息
    this.messageRouter.sendToConnection(connectionId, {
      type: 'connected',
      payload: {
        connectionId,
        userId,
        serverTime: Date.now()
      },
      timestamp: Date.now()
    });

    // 监听消息
    ws.on('message', async (data) => {
      try {
        const message = JSON.parse(data.toString());
        await this.messageRouter.route(message, connectionId);
      } catch (error) {
        console.error('[WebSocketServer] 消息解析错误:', error);
      }
    });

    // 监听 PONG
    ws.on('pong', () => {
      this.connectionManager.handlePing(connectionId);
    });

    // 监听关闭
    ws.on('close', () => {
      this.connectionManager.unregisterConnection(connectionId);
    });

    // 监听错误
    ws.on('error', (error) => {
      console.error(`[WebSocketServer] 连接错误 (${connectionId}):`, error);
      this.eventDispatcher.emit(SystemEventType.CONNECTION_ERROR, {
        connectionId,
        error: error.message
      });
    });
  }

  /**
   * 获取服务器状态
   * @returns {Object} 服务器状态
   */
  getStatus() {
    const connStats = this.connectionManager.getStats();
    const roomStats = this.roomManager.getStats();

    return {
      isRunning: this.status.isRunning,
      port: this.status.port,
      host: this.status.host,
      startedAt: this.status.startedAt,
      totalConnections: connStats.total,
      totalRooms: roomStats.totalRooms
    };
  }

  /**
   * 获取连接管理器实例
   * @returns {ConnectionManager}
   */
  getConnectionManager() {
    return this.connectionManager;
  }

  /**
   * 获取消息路由器实例
   * @returns {MessageRouter}
   */
  getMessageRouter() {
    return this.messageRouter;
  }

  /**
   * 获取房间管理器实例
   * @returns {RoomManager}
   */
  getRoomManager() {
    return this.roomManager;
  }

  /**
   * 获取事件分发器实例
   * @returns {EventDispatcher}
   */
  getEventDispatcher() {
    return this.eventDispatcher;
  }

  /**
   * 广播消息给所有连接
   * @param {Object} message - 消息对象
   * @param {string[]} excludeConnectionIds - 排除的连接ID列表
   * @returns {Promise<number>} 成功发送的数量
   */
  async broadcast(message, excludeConnectionIds = []) {
    return await this.messageRouter.broadcast(message, excludeConnectionIds);
  }

  /**
   * 监听事件
   * @param {string} eventType - 事件类型
   * @param {Function} handler - 事件处理器
   */
  on(eventType, handler) {
    this.eventDispatcher.on(eventType, handler);
  }
}

// ==================== 导出 ====================

export { WebSocketServer, SystemEventType };
export default WebSocketServer;

// ==================== 示例用法（可选） ====================

if (import.meta.url === `file://${process.argv[1]}`) {
  const server = new WebSocketServer({
    port: 8080,
    heartbeatInterval: 30000
  });

  server.on(SystemEventType.SERVER_STARTED, ({ port, host }) => {
    console.log(`\n服务器启动成功: ws://${host}:${port}\n`);
  });

  server.on(SystemEventType.CONNECTION_OPENED, ({ connectionId, userId }) => {
    console.log(`新连接: ${connectionId} (用户: ${userId || '匿名'})`);
  });

  server.start().catch((error) => {
    console.error('启动失败:', error);
    process.exit(1);
  });

  // 优雅退出
  process.on('SIGINT', async () => {
    console.log('\n正在关闭...');
    await server.stop();
    process.exit(0);
  });
}
