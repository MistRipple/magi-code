/**
 * WebSocket 服务器核心模块
 * 提供连接管理、心跳检测、消息广播、单播功能
 */

import { WebSocketServer } from 'ws';
import ConnectionManager from './connection-manager.js';

class WebSocketServerCore {
  constructor(options = {}) {
    // 配置参数
    this.port = options.port || 8080;
    this.heartbeatInterval = options.heartbeatInterval || 30000; // 心跳间隔 30 秒
    this.heartbeatTimeout = options.heartbeatTimeout || 35000; // 心跳超时 35 秒

    // 核心组件
    this.wss = null;
    this.connectionManager = new ConnectionManager();
    this.heartbeatTimer = null;

    // 事件回调
    this.onConnectionCallback = null;
    this.onMessageCallback = null;
    this.onCloseCallback = null;
    this.onErrorCallback = null;
  }

  /**
   * 启动 WebSocket 服务
   */
  start() {
    return new Promise((resolve, reject) => {
      try {
        // 创建 WebSocket 服务器
        this.wss = new WebSocketServer({ port: this.port });

        this.wss.on('listening', () => {
          console.log(`[WebSocketServer] 服务已启动，监听端口: ${this.port}`);
          
          // 启动心跳检测
          this.startHeartbeat();
          
          resolve();
        });

        this.wss.on('error', (error) => {
          console.error('[WebSocketServer] 服务器错误:', error);
          reject(error);
        });

        // 处理新连接
        this.wss.on('connection', (ws, req) => {
          this.handleConnection(ws, req);
        });

      } catch (error) {
        console.error('[WebSocketServer] 启动失败:', error);
        reject(error);
      }
    });
  }

  /**
   * 处理新连接
   * @param {WebSocket} ws - WebSocket 连接
   * @param {Object} req - HTTP 请求对象
   */
  handleConnection(ws, req) {
    // 添加到连接池
    const connectionId = this.connectionManager.addConnection(ws, {
      ip: req.socket.remoteAddress,
      userAgent: req.headers['user-agent']
    });

    // 发送欢迎消息
    this.sendToClient(connectionId, {
      type: 'connected',
      connectionId,
      message: '连接成功',
      timestamp: Date.now()
    });

    // 触发连接回调
    if (this.onConnectionCallback) {
      this.onConnectionCallback(connectionId, req);
    }

    // 监听客户端消息
    ws.on('message', (data) => {
      this.handleMessage(connectionId, data);
    });

    // 监听 pong 响应（心跳回复）
    ws.on('pong', () => {
      this.connectionManager.updateHeartbeat(connectionId);
    });

    // 监听连接关闭
    ws.on('close', (code, reason) => {
      console.log(`[WebSocketServer] 连接关闭: ${connectionId}, 代码: ${code}, 原因: ${reason}`);
      this.connectionManager.removeConnection(connectionId);
      
      if (this.onCloseCallback) {
        this.onCloseCallback(connectionId, code, reason);
      }
    });

    // 监听错误
    ws.on('error', (error) => {
      console.error(`[WebSocketServer] 连接错误 ${connectionId}:`, error);
      
      if (this.onErrorCallback) {
        this.onErrorCallback(connectionId, error);
      }
    });
  }

  /**
   * 处理客户端消息
   * @param {string} connectionId - 连接ID
   * @param {Buffer|String} data - 消息数据
   */
  handleMessage(connectionId, data) {
    try {
      const message = JSON.parse(data.toString());
      console.log(`[WebSocketServer] 收到消息 from ${connectionId}:`, message);

      // 处理心跳消息
      if (message.type === 'ping') {
        this.sendToClient(connectionId, {
          type: 'pong',
          timestamp: Date.now()
        });
        return;
      }

      // 触发消息回调
      if (this.onMessageCallback) {
        this.onMessageCallback(connectionId, message);
      }

    } catch (error) {
      console.error(`[WebSocketServer] 消息解析失败 ${connectionId}:`, error);
      this.sendToClient(connectionId, {
        type: 'error',
        message: '消息格式错误',
        error: error.message
      });
    }
  }

  /**
   * 发送消息给指定客户端（单播）
   * @param {string} connectionId - 连接ID
   * @param {Object} data - 消息数据
   * @returns {boolean} 是否发送成功
   */
  sendToClient(connectionId, data) {
    const conn = this.connectionManager.getConnection(connectionId);
    
    if (!conn) {
      console.warn(`[WebSocketServer] 连接不存在: ${connectionId}`);
      return false;
    }

    if (conn.ws.readyState !== 1) { // 1 = OPEN
      console.warn(`[WebSocketServer] 连接未就绪: ${connectionId}, 状态: ${conn.ws.readyState}`);
      return false;
    }

    try {
      const message = JSON.stringify(data);
      conn.ws.send(message);
      return true;
    } catch (error) {
      console.error(`[WebSocketServer] 发送失败 to ${connectionId}:`, error);
      return false;
    }
  }

  /**
   * 广播消息给所有客户端
   * @param {Object} data - 消息数据
   * @param {Array} excludeIds - 排除的连接ID数组（可选）
   * @returns {Object} 发送统计 { success: number, failed: number }
   */
  broadcast(data, excludeIds = []) {
    const connections = this.connectionManager.getAllConnections();
    const stats = { success: 0, failed: 0 };

    connections.forEach(conn => {
      // 跳过排除列表中的连接
      if (excludeIds.includes(conn.id)) {
        return;
      }

      const sent = this.sendToClient(conn.id, data);
      if (sent) {
        stats.success++;
      } else {
        stats.failed++;
      }
    });

    console.log(`[WebSocketServer] 广播完成: 成功 ${stats.success}, 失败 ${stats.failed}`);
    return stats;
  }

  /**
   * 启动心跳检测
   */
  startHeartbeat() {
    this.heartbeatTimer = setInterval(() => {
      const connections = this.connectionManager.getAllConnections();
      
      connections.forEach(conn => {
        // 检查上次心跳是否超时
        if (Date.now() - conn.lastHeartbeat > this.heartbeatTimeout) {
          console.warn(`[WebSocketServer] 心跳超时，关闭连接: ${conn.id}`);
          conn.ws.terminate();
          this.connectionManager.removeConnection(conn.id);
          return;
        }

        // 发送 ping
        if (conn.ws.readyState === 1) {
          conn.ws.ping();
        }
      });

    }, this.heartbeatInterval);

    console.log(`[WebSocketServer] 心跳检测已启动，间隔: ${this.heartbeatInterval}ms`);
  }

  /**
   * 停止心跳检测
   */
  stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
      console.log('[WebSocketServer] 心跳检测已停止');
    }
  }

  /**
   * 注册连接事件回调
   * @param {Function} callback - 回调函数 (connectionId, req) => {}
   */
  onConnection(callback) {
    this.onConnectionCallback = callback;
  }

  /**
   * 注册消息事件回调
   * @param {Function} callback - 回调函数 (connectionId, message) => {}
   */
  onMessage(callback) {
    this.onMessageCallback = callback;
  }

  /**
   * 注册关闭事件回调
   * @param {Function} callback - 回调函数 (connectionId, code, reason) => {}
   */
  onClose(callback) {
    this.onCloseCallback = callback;
  }

  /**
   * 注册错误事件回调
   * @param {Function} callback - 回调函数 (connectionId, error) => {}
   */
  onError(callback) {
    this.onErrorCallback = callback;
  }

  /**
   * 获取服务器统计信息
   * @returns {Object} 统计信息
   */
  getStats() {
    return {
      ...this.connectionManager.getStats(),
      port: this.port,
      uptime: this.wss ? process.uptime() : 0
    };
  }

  /**
   * 关闭服务器
   */
  async close() {
    console.log('[WebSocketServer] 正在关闭服务器...');
    
    // 停止心跳
    this.stopHeartbeat();

    // 关闭所有连接
    const connections = this.connectionManager.getAllConnections();
    connections.forEach(conn => {
      conn.ws.close(1000, '服务器关闭');
    });

    // 关闭服务器
    if (this.wss) {
      return new Promise((resolve) => {
        this.wss.close(() => {
          console.log('[WebSocketServer] 服务器已关闭');
          resolve();
        });
      });
    }
  }
}

export default WebSocketServerCore;
