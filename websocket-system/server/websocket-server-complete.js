/**
 * WebSocket 服务端完整实现
 * 集成连接管理、房间管理、消息路由等核心功能
 */

import { WebSocketServer } from 'ws';
import { createServer } from 'http';
import ConnectionManager from './connection-manager.js';
import RoomManager from './room-manager.js';
import MessageRouter from './message-router.js';

class WebSocketServerComplete {
  constructor(options = {}) {
    // 配置参数
    this.config = {
      port: options.port || 8080,
      host: options.host || '0.0.0.0',
      heartbeatInterval: options.heartbeatInterval || 30000,
      heartbeatTimeout: options.heartbeatTimeout || 35000,
      logLevel: options.logLevel || 'info'
    };

    // 核心组件
    this.httpServer = null;
    this.wss = null;
    this.connectionManager = new ConnectionManager();
    this.roomManager = new RoomManager();
    this.messageRouter = new MessageRouter(this.connectionManager, this.roomManager);
    
    // 心跳定时器
    this.heartbeatTimer = null;
    
    // 服务器状态
    this.isRunning = false;
    this.startedAt = null;

    // 事件回调
    this.eventCallbacks = {
      onServerStart: [],
      onServerStop: [],
      onConnection: [],
      onDisconnect: [],
      onMessage: [],
      onError: []
    };
  }

  /**
   * 启动服务器
   */
  async start() {
    if (this.isRunning) {
      throw new Error('服务器已在运行中');
    }

    return new Promise((resolve, reject) => {
      try {
        // 创建 HTTP 服务器
        this.httpServer = createServer((req, res) => {
          res.writeHead(200, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({
            service: 'WebSocket Server',
            version: '1.0.0',
            status: 'running',
            uptime: this.getUptime(),
            stats: this.getStats()
          }, null, 2));
        });

        // 创建 WebSocket 服务器
        this.wss = new WebSocketServer({ server: this.httpServer });

        // 监听连接事件
        this.wss.on('connection', (ws, req) => {
          this.handleConnection(ws, req);
        });

        // 监听服务器错误
        this.wss.on('error', (error) => {
          this.log('error', '服务器错误', error);
          this.emitEvent('onError', { type: 'server', error });
        });

        // 启动 HTTP 服务器
        this.httpServer.listen(this.config.port, this.config.host, () => {
          this.isRunning = true;
          this.startedAt = Date.now();
          
          this.log('info', `✅ WebSocket 服务器启动成功`);
          this.log('info', `📡 监听地址: ws://${this.config.host}:${this.config.port}`);
          this.log('info', `💓 心跳间隔: ${this.config.heartbeatInterval}ms`);
          
          // 启动心跳检测
          this.startHeartbeat();
          
          // 触发事件
          this.emitEvent('onServerStart', {
            port: this.config.port,
            host: this.config.host
          });
          
          resolve();
        });

        // 监听 HTTP 服务器错误
        this.httpServer.on('error', (error) => {
          this.log('error', 'HTTP 服务器错误', error);
          reject(error);
        });

      } catch (error) {
        this.log('error', '启动失败', error);
        reject(error);
      }
    });
  }

  /**
   * 处理新连接
   */
  handleConnection(ws, req) {
    // 提取用户信息（实际应用中应从认证令牌中获取）
    const urlParams = new URL(req.url, `http://${req.headers.host}`).searchParams;
    const userId = urlParams.get('userId') || `user_${Date.now()}`;
    
    // 添加到连接池
    const connectionId = this.connectionManager.addConnection(ws, {
      ip: req.socket.remoteAddress,
      userAgent: req.headers['user-agent'],
      userId
    });

    this.log('info', `✅ 新连接`, {
      connectionId,
      userId,
      ip: req.socket.remoteAddress
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

    // 触发连接事件
    this.emitEvent('onConnection', { connectionId, userId, req });

    // 监听客户端消息
    ws.on('message', (data) => {
      this.handleMessage(connectionId, userId, data);
    });

    // 监听 pong 响应（心跳回复）
    ws.on('pong', () => {
      this.connectionManager.updateHeartbeat(connectionId);
      this.log('debug', `💓 心跳响应`, { connectionId });
    });

    // 监听连接关闭
    ws.on('close', (code, reason) => {
      this.handleDisconnect(connectionId, userId, code, reason);
    });

    // 监听错误
    ws.on('error', (error) => {
      this.log('error', `连接错误`, { connectionId, error: error.message });
      this.emitEvent('onError', { type: 'connection', connectionId, error });
    });
  }

  /**
   * 处理客户端消息
   */
  handleMessage(connectionId, userId, data) {
    try {
      const message = JSON.parse(data.toString());
      
      this.log('debug', `📨 收到消息`, {
        connectionId,
        userId,
        type: message.type
      });

      // 触发消息事件
      this.emitEvent('onMessage', { connectionId, userId, message });

      // 路由消息到对应处理器
      this.messageRouter.route(message, connectionId, userId);

    } catch (error) {
      this.log('error', `消息解析失败`, {
        connectionId,
        error: error.message
      });
      
      this.messageRouter.sendError(
        connectionId,
        null,
        'PARSE_ERROR',
        '消息格式错误'
      );
    }
  }

  /**
   * 处理连接断开
   */
  handleDisconnect(connectionId, userId, code, reason) {
    this.log('info', `❌ 连接断开`, {
      connectionId,
      userId,
      code,
      reason: reason.toString()
    });

    // 从所有房间移除
    this.roomManager.removeUserFromAllRooms(userId, connectionId);

    // 从连接池移除
    this.connectionManager.removeConnection(connectionId);

    // 触发断开事件
    this.emitEvent('onDisconnect', { connectionId, userId, code, reason });
  }

  /**
   * 启动心跳检测
   */
  startHeartbeat() {
    this.heartbeatTimer = setInterval(() => {
      const connections = this.connectionManager.getAllConnections();
      const now = Date.now();
      
      connections.forEach(conn => {
        // 检查心跳超时
        if (now - conn.lastHeartbeat > this.config.heartbeatTimeout) {
          this.log('warn', `💔 心跳超时，关闭连接`, { connectionId: conn.id });
          conn.ws.terminate();
          this.handleDisconnect(conn.id, conn.metadata.userId, 4000, '心跳超时');
          return;
        }

        // 发送心跳 ping
        if (conn.ws.readyState === 1) {
          conn.ws.ping();
        }
      });

    }, this.config.heartbeatInterval);

    this.log('info', `💓 心跳检测已启动`);
  }

  /**
   * 停止心跳检测
   */
  stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
      this.log('info', `💔 心跳检测已停止`);
    }
  }

  /**
   * 停止服务器
   */
  async stop() {
    if (!this.isRunning) {
      return;
    }

    this.log('info', '🛑 正在关闭服务器...');

    // 停止心跳
    this.stopHeartbeat();

    // 关闭所有连接
    const connections = this.connectionManager.getAllConnections();
    connections.forEach(conn => {
      this.messageRouter.sendToConnection(conn.id, {
        type: 'notification',
        payload: {
          level: 'warning',
          message: '服务器正在关闭'
        },
        timestamp: Date.now()
      });
      conn.ws.close(1000, '服务器关闭');
    });

    // 关闭 WebSocket 服务器
    if (this.wss) {
      await new Promise((resolve) => {
        this.wss.close(() => {
          this.log('info', '✅ WebSocket 服务器已关闭');
          resolve();
        });
      });
    }

    // 关闭 HTTP 服务器
    if (this.httpServer) {
      await new Promise((resolve) => {
        this.httpServer.close(() => {
          this.log('info', '✅ HTTP 服务器已关闭');
          resolve();
        });
      });
    }

    this.isRunning = false;
    this.emitEvent('onServerStop', {});
  }

  /**
   * 注册事件监听器
   */
  on(eventName, callback) {
    if (this.eventCallbacks[eventName]) {
      this.eventCallbacks[eventName].push(callback);
    }
  }

  /**
   * 触发事件
   */
  emitEvent(eventName, data) {
    if (this.eventCallbacks[eventName]) {
      this.eventCallbacks[eventName].forEach(callback => {
        try {
          callback(data);
        } catch (error) {
          this.log('error', `事件回调执行失败: ${eventName}`, error);
        }
      });
    }
  }

  /**
   * 获取服务器统计信息
   */
  getStats() {
    return {
      server: {
        isRunning: this.isRunning,
        port: this.config.port,
        host: this.config.host,
        uptime: this.getUptime()
      },
      connections: this.connectionManager.getStats(),
      rooms: this.roomManager.getStats()
    };
  }

  /**
   * 获取运行时长（秒）
   */
  getUptime() {
    return this.startedAt ? Math.floor((Date.now() - this.startedAt) / 1000) : 0;
  }

  /**
   * 日志输出
   */
  log(level, message, data = {}) {
    const levels = { error: 0, warn: 1, info: 2, debug: 3 };
    const configLevel = levels[this.config.logLevel] || 2;
    const msgLevel = levels[level] || 2;

    if (msgLevel <= configLevel) {
      const timestamp = new Date().toISOString();
      const dataStr = Object.keys(data).length > 0 ? JSON.stringify(data) : '';
      console.log(`[${timestamp}] [${level.toUpperCase()}] ${message} ${dataStr}`);
    }
  }

  /**
   * 获取核心服务实例（用于高级扩展）
   */
  getConnectionManager() {
    return this.connectionManager;
  }

  getRoomManager() {
    return this.roomManager;
  }

  getMessageRouter() {
    return this.messageRouter;
  }
}

export default WebSocketServerComplete;
