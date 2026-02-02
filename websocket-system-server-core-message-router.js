/**
 * 消息路由模块
 * 负责将消息路由到指定的客户端
 */

const { parseMessage, MessageType } = require('../protocol/message-protocol');
const logger = require('../utils/logger');

/**
 * 消息路由器类
 */
class MessageRouter {
  constructor(connectionManager, broadcastManager) {
    this.connectionManager = connectionManager;
    this.broadcastManager = broadcastManager;
    
    // 消息处理器映射
    this.messageHandlers = new Map();
    
    // 注册默认消息处理器
    this.registerDefaultHandlers();
  }

  /**
   * 注册默认消息处理器
   */
  registerDefaultHandlers() {
    // 心跳请求处理
    this.registerHandler(MessageType.PING, (clientId, message) => {
      this.connectionManager.respondPong(clientId);
    });
    
    // 点对点消息处理
    this.registerHandler(MessageType.MESSAGE, (clientId, message) => {
      this.handleP2PMessage(clientId, message);
    });
    
    // 广播消息处理
    this.registerHandler(MessageType.BROADCAST, (clientId, message) => {
      this.handleBroadcastMessage(clientId, message);
    });
  }

  /**
   * 注册消息类型处理器
   * @param {string} messageType - 消息类型
   * @param {Function} handler - 处理函数 (clientId, message) => void
   */
  registerHandler(messageType, handler) {
    this.messageHandlers.set(messageType, handler);
  }

  /**
   * 移除消息类型处理器
   * @param {string} messageType - 消息类型
   */
  unregisterHandler(messageType) {
    this.messageHandlers.delete(messageType);
  }

  /**
   * 路由消息
   * @param {string} clientId - 发送者客户端 ID
   * @param {string} rawMessage - 原始消息字符串
   */
  routeMessage(clientId, rawMessage) {
    // 解析消息
    const message = parseMessage(rawMessage);
    
    if (!message) {
      logger.warn(`无效消息格式, 来自客户端: ${clientId}`);
      return;
    }
    
    logger.debug(`收到消息: 类型=${message.type}, 来自=${clientId}`);
    
    // 查找对应的处理器
    const handler = this.messageHandlers.get(message.type);
    
    if (handler) {
      try {
        handler(clientId, message);
      } catch (error) {
        logger.error(`消息处理出错: 类型=${message.type}, 客户端=${clientId}`, error);
      }
    } else {
      logger.warn(`未找到消息处理器: 类型=${message.type}`);
    }
  }

  /**
   * 处理点对点消息
   * @param {string} fromClientId - 发送者 ID
   * @param {object} message - 消息对象
   */
  handleP2PMessage(fromClientId, message) {
    const { to, payload } = message;
    
    if (!to) {
      logger.warn(`点对点消息缺少接收者, 来自客户端: ${fromClientId}`);
      return;
    }
    
    // 检查接收者是否在线
    const receiverExists = this.connectionManager.getClientInfo(to);
    
    if (!receiverExists) {
      logger.warn(`接收者不在线: ${to}`);
      
      // 可以在这里实现离线消息存储逻辑
      // this.storeOfflineMessage(to, message);
      
      return;
    }
    
    // 转发消息
    const forwardedMessage = {
      ...message,
      from: fromClientId // 确保包含发送者信息
    };
    
    const success = this.connectionManager.sendToClient(to, forwardedMessage);
    
    if (success) {
      logger.info(`点对点消息已转发: ${fromClientId} -> ${to}`);
    } else {
      logger.error(`点对点消息转发失败: ${fromClientId} -> ${to}`);
    }
  }

  /**
   * 处理广播消息
   * @param {string} fromClientId - 发送者 ID
   * @param {object} message - 消息对象
   */
  handleBroadcastMessage(fromClientId, message) {
    // 设置发送者信息
    const broadcastMessage = {
      ...message,
      from: fromClientId
    };
    
    // 使用广播管理器进行广播
    this.broadcastManager.broadcast(broadcastMessage, fromClientId);
    
    logger.info(`广播消息已发送, 来自客户端: ${fromClientId}`);
  }

  /**
   * 获取已注册的消息类型列表
   * @returns {string[]} 消息类型数组
   */
  getRegisteredMessageTypes() {
    return Array.from(this.messageHandlers.keys());
  }
}

module.exports = MessageRouter;
