/**
 * 消息路由器
 * 负责根据消息类型分发到不同的处理器，实现广播、点对点、房间消息路由
 */

class MessageRouter {
  constructor(connectionManager, roomManager) {
    this.connectionManager = connectionManager;
    this.roomManager = roomManager;
    
    // 消息处理器映射：Map<messageType, handler>
    this.handlers = new Map();
    
    // 注册默认处理器
    this.registerDefaultHandlers();
  }

  /**
   * 注册默认消息处理器
   */
  registerDefaultHandlers() {
    // 心跳处理
    this.registerHandler('ping', (message, connectionId) => {
      this.sendToConnection(connectionId, {
        type: 'pong',
        timestamp: Date.now(),
        replyTo: message.id
      });
    });

    // 加入房间
    this.registerHandler('join_room', (message, connectionId, userId) => {
      const { roomId, password } = message.payload || {};
      
      if (!roomId) {
        return this.sendError(connectionId, message.id, 'MISSING_ROOM_ID', '缺少房间ID');
      }

      const result = this.roomManager.joinRoom(roomId, userId, connectionId);
      
      if (result.success) {
        // 通知自己加入成功
        this.sendToConnection(connectionId, {
          type: 'room_joined',
          payload: {
            roomId,
            members: result.members,
            memberCount: result.memberCount,
            metadata: this.roomManager.getRoomMetadata(roomId)
          },
          replyTo: message.id,
          timestamp: Date.now()
        });

        // 通知房间内其他成员
        this.sendToRoom(roomId, {
          type: 'user_joined',
          payload: {
            roomId,
            userId,
            joinedAt: Date.now()
          },
          timestamp: Date.now()
        }, [connectionId]);
      } else {
        this.sendError(connectionId, message.id, 'JOIN_ROOM_FAILED', result.reason || '加入房间失败');
      }
    });

    // 离开房间
    this.registerHandler('leave_room', (message, connectionId, userId) => {
      const { roomId } = message.payload || {};
      
      if (!roomId) {
        return this.sendError(connectionId, message.id, 'MISSING_ROOM_ID', '缺少房间ID');
      }

      const success = this.roomManager.leaveRoom(roomId, userId, connectionId);
      
      if (success) {
        // 通知自己离开成功
        this.sendToConnection(connectionId, {
          type: 'room_left',
          payload: { roomId },
          replyTo: message.id,
          timestamp: Date.now()
        });

        // 通知房间内其他成员（如果房间还存在）
        if (this.roomManager.roomExists(roomId)) {
          this.sendToRoom(roomId, {
            type: 'user_left',
            payload: {
              roomId,
              userId,
              leftAt: Date.now()
            },
            timestamp: Date.now()
          });
        }
      } else {
        this.sendError(connectionId, message.id, 'LEAVE_ROOM_FAILED', '离开房间失败');
      }
    });

    // 创建房间
    this.registerHandler('create_room', (message, connectionId, userId) => {
      const { roomId, name, description, maxMembers, isPrivate } = message.payload || {};
      
      if (!roomId) {
        return this.sendError(connectionId, message.id, 'MISSING_ROOM_ID', '缺少房间ID');
      }

      const created = this.roomManager.createRoom(roomId, {
        name,
        description,
        maxMembers,
        isPrivate,
        createdBy: userId
      });

      if (created) {
        // 创建者自动加入房间
        this.roomManager.joinRoom(roomId, userId, connectionId);

        this.sendToConnection(connectionId, {
          type: 'room_created',
          payload: {
            roomId,
            metadata: this.roomManager.getRoomMetadata(roomId)
          },
          replyTo: message.id,
          timestamp: Date.now()
        });
      } else {
        this.sendError(connectionId, message.id, 'ROOM_EXISTS', '房间已存在');
      }
    });

    // 发送消息（点对点或房间）
    this.registerHandler('send_message', (message, connectionId, userId) => {
      const { targetType, targetId, content, contentType } = message.payload || {};
      
      if (!targetType || !targetId) {
        return this.sendError(connectionId, message.id, 'INVALID_TARGET', '无效的目标');
      }

      const messagePayload = {
        messageId: this.generateMessageId(),
        fromUserId: userId,
        targetType,
        targetId,
        content,
        contentType: contentType || 'text',
        sentAt: Date.now()
      };

      if (targetType === 'user') {
        // 点对点消息
        this.sendToUser(targetId, {
          type: 'message',
          payload: messagePayload,
          timestamp: Date.now()
        });

        // 发送确认
        this.sendToConnection(connectionId, {
          type: 'message_sent',
          payload: { messageId: messagePayload.messageId },
          replyTo: message.id,
          timestamp: Date.now()
        });
      } else if (targetType === 'room') {
        // 房间消息
        if (!this.roomManager.isUserInRoom(targetId, userId)) {
          return this.sendError(connectionId, message.id, 'NOT_IN_ROOM', '您不在该房间中');
        }

        this.sendToRoom(targetId, {
          type: 'message',
          payload: messagePayload,
          metadata: { roomId: targetId },
          timestamp: Date.now()
        }, [connectionId]); // 排除发送者

        // 发送确认
        this.sendToConnection(connectionId, {
          type: 'message_sent',
          payload: { messageId: messagePayload.messageId },
          replyTo: message.id,
          timestamp: Date.now()
        });
      }
    });

    // 广播消息
    this.registerHandler('broadcast', (message, connectionId, userId) => {
      const { content } = message.payload || {};
      
      this.broadcast({
        type: 'broadcast',
        payload: {
          content,
          fromUserId: userId,
          sentAt: Date.now()
        },
        timestamp: Date.now()
      }, [connectionId]);

      // 发送确认
      this.sendToConnection(connectionId, {
        type: 'broadcast_sent',
        replyTo: message.id,
        timestamp: Date.now()
      });
    });

    // 获取房间成员
    this.registerHandler('get_room_members', (message, connectionId) => {
      const { roomId } = message.payload || {};
      
      if (!roomId) {
        return this.sendError(connectionId, message.id, 'MISSING_ROOM_ID', '缺少房间ID');
      }

      const members = this.roomManager.getRoomMembers(roomId);
      
      this.sendToConnection(connectionId, {
        type: 'room_members',
        payload: {
          roomId,
          members
        },
        replyTo: message.id,
        timestamp: Date.now()
      });
    });

    // 获取用户房间列表
    this.registerHandler('get_user_rooms', (message, connectionId, userId) => {
      const { userId: targetUserId } = message.payload || {};
      const queryUserId = targetUserId || userId;
      
      const rooms = this.roomManager.getUserRooms(queryUserId);
      
      this.sendToConnection(connectionId, {
        type: 'user_rooms',
        payload: {
          userId: queryUserId,
          rooms
        },
        replyTo: message.id,
        timestamp: Date.now()
      });
    });
  }

  /**
   * 注册消息处理器
   * @param {string} messageType - 消息类型
   * @param {Function} handler - 处理函数 (message, connectionId, userId) => {}
   */
  registerHandler(messageType, handler) {
    this.handlers.set(messageType, handler);
    console.log(`[MessageRouter] 注册处理器: ${messageType}`);
  }

  /**
   * 注销消息处理器
   * @param {string} messageType - 消息类型
   */
  unregisterHandler(messageType) {
    this.handlers.delete(messageType);
    console.log(`[MessageRouter] 注销处理器: ${messageType}`);
  }

  /**
   * 路由消息到对应处理器
   * @param {Object} message - 客户端消息
   * @param {string} connectionId - 连接ID
   * @param {string} userId - 用户ID
   */
  async route(message, connectionId, userId) {
    const { type } = message;
    
    const handler = this.handlers.get(type);
    if (handler) {
      try {
        await handler(message, connectionId, userId);
      } catch (error) {
        console.error(`[MessageRouter] 处理消息失败 (${type}):`, error);
        this.sendError(connectionId, message.id, 'HANDLER_ERROR', '消息处理失败');
      }
    } else {
      console.warn(`[MessageRouter] 未知消息类型: ${type}`);
      this.sendError(connectionId, message.id, 'UNKNOWN_TYPE', `未知消息类型: ${type}`);
    }
  }

  /**
   * 发送消息到指定连接
   * @param {string} connectionId - 连接ID
   * @param {Object} message - 服务端消息
   * @returns {boolean}
   */
  sendToConnection(connectionId, message) {
    const conn = this.connectionManager.getConnection(connectionId);
    if (!conn || conn.ws.readyState !== 1) {
      return false;
    }

    try {
      const data = JSON.stringify({
        id: this.generateMessageId(),
        version: '1.0',
        ...message
      });
      conn.ws.send(data);
      return true;
    } catch (error) {
      console.error(`[MessageRouter] 发送失败 to ${connectionId}:`, error);
      return false;
    }
  }

  /**
   * 发送消息给指定用户（所有连接）
   * @param {string} userId - 用户ID
   * @param {Object} message - 服务端消息
   * @returns {number} 成功发送的连接数
   */
  sendToUser(userId, message) {
    const connections = this.connectionManager.getAllConnections();
    let count = 0;

    connections.forEach(conn => {
      if (conn.metadata.userId === userId) {
        if (this.sendToConnection(conn.id, message)) {
          count++;
        }
      }
    });

    return count;
  }

  /**
   * 广播消息给所有连接
   * @param {Object} message - 服务端消息
   * @param {Array} excludeConnectionIds - 排除的连接ID列表
   * @returns {Object} 发送统计
   */
  broadcast(message, excludeConnectionIds = []) {
    const connections = this.connectionManager.getAllConnections();
    const stats = { success: 0, failed: 0 };

    connections.forEach(conn => {
      if (excludeConnectionIds.includes(conn.id)) {
        return;
      }

      if (this.sendToConnection(conn.id, message)) {
        stats.success++;
      } else {
        stats.failed++;
      }
    });

    console.log(`[MessageRouter] 广播完成: 成功 ${stats.success}, 失败 ${stats.failed}`);
    return stats;
  }

  /**
   * 发送消息到房间
   * @param {string} roomId - 房间ID
   * @param {Object} message - 服务端消息
   * @param {Array} excludeConnectionIds - 排除的连接ID列表
   * @returns {number} 成功发送的数量
   */
  sendToRoom(roomId, message, excludeConnectionIds = []) {
    const connectionIds = this.roomManager.getRoomConnectionIds(roomId);
    let count = 0;

    connectionIds.forEach(connId => {
      if (excludeConnectionIds.includes(connId)) {
        return;
      }

      if (this.sendToConnection(connId, message)) {
        count++;
      }
    });

    console.log(`[MessageRouter] 房间消息发送完成 (${roomId}): ${count} 个连接`);
    return count;
  }

  /**
   * 发送错误消息
   * @param {string} connectionId - 连接ID
   * @param {string} replyTo - 关联的消息ID
   * @param {string} code - 错误码
   * @param {string} message - 错误描述
   */
  sendError(connectionId, replyTo, code, message) {
    this.sendToConnection(connectionId, {
      type: 'error',
      payload: {
        code,
        message
      },
      replyTo,
      timestamp: Date.now()
    });
  }

  /**
   * 生成消息ID
   * @returns {string}
   */
  generateMessageId() {
    return `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
  }
}

export default MessageRouter;
