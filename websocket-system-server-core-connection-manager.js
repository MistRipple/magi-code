/**
 * 连接管理器模块
 * 负责管理所有 WebSocket 客户端连接，包括连接建立、断开、心跳检测等
 */

const { v4: uuidv4 } = require('uuid');
const { createSystemMessage, createPongMessage, serializeMessage, SystemMessageType } = require('../protocol/message-protocol');
const logger = require('../utils/logger');

/**
 * 连接管理器类
 */
class ConnectionManager {
  constructor(options = {}) {
    // 存储所有活跃连接 Map<clientId, { ws, metadata }>
    this.connections = new Map();
    
    // 心跳检测配置
    this.heartbeatInterval = options.heartbeatInterval || 30000; // 30秒
    this.heartbeatTimeout = options.heartbeatTimeout || 60000;   // 60秒超时
    
    // 心跳检测定时器
    this.heartbeatTimer = null;
    
    // 连接事件回调
    this.onClientConnected = options.onClientConnected || null;
    this.onClientDisconnected = options.onClientDisconnected || null;
    this.onMessageReceived = options.onMessageReceived || null;
  }

  /**
   * 启动连接管理器
   */
  start() {
    // 启动心跳检测
    this.startHeartbeat();
    logger.info('连接管理器已启动');
  }

  /**
   * 停止连接管理器
   */
  stop() {
    // 停止心跳检测
    this.stopHeartbeat();
    
    // 断开所有连接
    this.disconnectAll();
    
    logger.info('连接管理器已停止');
  }

  /**
   * 添加新连接
   * @param {WebSocket} ws - WebSocket 连接对象
   * @param {object} metadata - 连接元数据（如 IP、用户代理等）
   * @returns {string} 客户端 ID
   */
  addConnection(ws, metadata = {}) {
    const clientId = uuidv4();
    
    // 存储连接信息
    this.connections.set(clientId, {
      ws,
      metadata: {
        ...metadata,
        connectedAt: Date.now(),
        lastHeartbeat: Date.now()
      }
    });
    
    // 设置 WebSocket 事件处理
    this.setupWebSocketHandlers(ws, clientId);
    
    // 发送连接成功消息
    const welcomeMessage = createSystemMessage(SystemMessageType.CONNECTED, {
      clientId,
      message: '连接成功',
      serverTime: Date.now()
    });
    this.sendToClient(clientId, welcomeMessage);
    
    logger.info(`客户端已连接: ${clientId}`);
    
    // 触发连接回调
    if (this.onClientConnected) {
      this.onClientConnected(clientId, metadata);
    }
    
    return clientId;
  }

  /**
   * 设置 WebSocket 事件处理器
   * @param {WebSocket} ws - WebSocket 对象
   * @param {string} clientId - 客户端 ID
   */
  setupWebSocketHandlers(ws, clientId) {
    // 接收消息
    ws.on('message', (rawMessage) => {
      this.handleMessage(clientId, rawMessage);
    });
    
    // 连接关闭
    ws.on('close', () => {
      this.removeConnection(clientId, 'close');
    });
    
    // 连接错误
    ws.on('error', (error) => {
      logger.error(`客户端 ${clientId} 发生错误:`, error);
      this.removeConnection(clientId, 'error');
    });
  }

  /**
   * 处理接收到的消息
   * @param {string} clientId - 客户端 ID
   * @param {string} rawMessage - 原始消息
   */
  handleMessage(clientId, rawMessage) {
    const connection = this.connections.get(clientId);
    if (!connection) {
      return;
    }
    
    // 更新最后心跳时间
    connection.metadata.lastHeartbeat = Date.now();
    
    // 触发消息接收回调
    if (this.onMessageReceived) {
      this.onMessageReceived(clientId, rawMessage);
    }
  }

  /**
   * 移除连接
   * @param {string} clientId - 客户端 ID
   * @param {string} reason - 断开原因
   */
  removeConnection(clientId, reason = 'unknown') {
    const connection = this.connections.get(clientId);
    if (!connection) {
      return;
    }
    
    // 关闭 WebSocket 连接
    try {
      if (connection.ws.readyState === 1) { // OPEN
        connection.ws.close();
      }
    } catch (error) {
      logger.error(`关闭连接时出错: ${clientId}`, error);
    }
    
    // 从连接池中删除
    this.connections.delete(clientId);
    
    logger.info(`客户端已断开: ${clientId}, 原因: ${reason}`);
    
    // 触发断开回调
    if (this.onClientDisconnected) {
      this.onClientDisconnected(clientId, reason);
    }
  }

  /**
   * 断开所有连接
   */
  disconnectAll() {
    const clientIds = Array.from(this.connections.keys());
    clientIds.forEach(clientId => {
      this.removeConnection(clientId, 'server_shutdown');
    });
  }

  /**
   * 向指定客户端发送消息
   * @param {string} clientId - 客户端 ID
   * @param {object} message - 消息对象
   * @returns {boolean} 是否发送成功
   */
  sendToClient(clientId, message) {
    const connection = this.connections.get(clientId);
    if (!connection) {
      logger.warn(`客户端不存在: ${clientId}`);
      return false;
    }
    
    try {
      const messageStr = serializeMessage(message);
      connection.ws.send(messageStr);
      return true;
    } catch (error) {
      logger.error(`发送消息失败: ${clientId}`, error);
      return false;
    }
  }

  /**
   * 获取所有在线客户端 ID
   * @returns {string[]} 客户端 ID 数组
   */
  getOnlineClients() {
    return Array.from(this.connections.keys());
  }

  /**
   * 获取在线客户端数量
   * @returns {number} 客户端数量
   */
  getClientCount() {
    return this.connections.size;
  }

  /**
   * 获取客户端连接信息
   * @param {string} clientId - 客户端 ID
   * @returns {object|null} 连接信息
   */
  getClientInfo(clientId) {
    const connection = this.connections.get(clientId);
    if (!connection) {
      return null;
    }
    
    return {
      clientId,
      metadata: connection.metadata
    };
  }

  /**
   * 启动心跳检测
   */
  startHeartbeat() {
    this.heartbeatTimer = setInterval(() => {
      this.checkHeartbeat();
    }, this.heartbeatInterval);
  }

  /**
   * 停止心跳检测
   */
  stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  /**
   * 检查所有连接的心跳状态
   */
  checkHeartbeat() {
    const now = Date.now();
    const timeoutClients = [];
    
    this.connections.forEach((connection, clientId) => {
      const timeSinceLastHeartbeat = now - connection.metadata.lastHeartbeat;
      
      if (timeSinceLastHeartbeat > this.heartbeatTimeout) {
        timeoutClients.push(clientId);
      }
    });
    
    // 断开超时的连接
    timeoutClients.forEach(clientId => {
      logger.warn(`客户端心跳超时: ${clientId}`);
      this.removeConnection(clientId, 'heartbeat_timeout');
    });
  }

  /**
   * 响应心跳请求
   * @param {string} clientId - 客户端 ID
   */
  respondPong(clientId) {
    const pongMessage = createPongMessage('server');
    this.sendToClient(clientId, pongMessage);
  }
}

module.exports = ConnectionManager;
