/**
 * 消息协议定义模块
 * 定义 WebSocket 通信的标准消息格式和消息类型
 */

const { v4: uuidv4 } = require('uuid');

/**
 * 消息类型枚举
 */
const MessageType = {
  PING: 'ping',              // 心跳请求
  PONG: 'pong',              // 心跳响应
  MESSAGE: 'message',        // 点对点消息
  BROADCAST: 'broadcast',    // 广播消息
  SYSTEM: 'system',          // 系统消息（连接成功、错误等）
  JOIN: 'join',              // 客户端加入
  LEAVE: 'leave'             // 客户端离开
};

/**
 * 系统消息子类型
 */
const SystemMessageType = {
  CONNECTED: 'connected',        // 连接成功
  ERROR: 'error',                // 错误
  CLIENT_LIST: 'client_list',    // 客户端列表更新
  DISCONNECTED: 'disconnected'   // 断开连接
};

/**
 * 创建标准消息对象
 * @param {string} type - 消息类型
 * @param {object} payload - 消息负载
 * @param {string} from - 发送者 ID
 * @param {string} to - 接收者 ID（可选，广播消息不需要）
 * @returns {object} 标准消息对象
 */
function createMessage(type, payload = {}, from = null, to = null) {
  return {
    type,                           // 消息类型
    payload,                        // 消息内容
    timestamp: Date.now(),          // 时间戳（毫秒）
    messageId: uuidv4(),            // 唯一消息 ID
    from,                           // 发送者客户端 ID
    to                              // 接收者客户端 ID（all 表示广播）
  };
}

/**
 * 创建心跳请求消息
 * @param {string} clientId - 客户端 ID
 * @returns {object} 心跳消息
 */
function createPingMessage(clientId) {
  return createMessage(MessageType.PING, {}, clientId);
}

/**
 * 创建心跳响应消息
 * @param {string} clientId - 客户端 ID
 * @returns {object} 心跳响应消息
 */
function createPongMessage(clientId) {
  return createMessage(MessageType.PONG, {}, clientId);
}

/**
 * 创建点对点消息
 * @param {string} from - 发送者 ID
 * @param {string} to - 接收者 ID
 * @param {object} content - 消息内容
 * @returns {object} 点对点消息
 */
function createP2PMessage(from, to, content) {
  return createMessage(MessageType.MESSAGE, content, from, to);
}

/**
 * 创建广播消息
 * @param {string} from - 发送者 ID
 * @param {object} content - 消息内容
 * @returns {object} 广播消息
 */
function createBroadcastMessage(from, content) {
  return createMessage(MessageType.BROADCAST, content, from, 'all');
}

/**
 * 创建系统消息
 * @param {string} subType - 系统消息子类型
 * @param {object} data - 系统消息数据
 * @returns {object} 系统消息
 */
function createSystemMessage(subType, data = {}) {
  return createMessage(MessageType.SYSTEM, {
    subType,
    data
  }, 'server');
}

/**
 * 验证消息格式是否合法
 * @param {object} message - 待验证的消息对象
 * @returns {boolean} 是否合法
 */
function validateMessage(message) {
  if (!message || typeof message !== 'object') {
    return false;
  }
  
  // 必须包含 type 和 timestamp
  if (!message.type || !message.timestamp) {
    return false;
  }
  
  // type 必须是有效的消息类型
  if (!Object.values(MessageType).includes(message.type)) {
    return false;
  }
  
  // MESSAGE 类型必须有 to 字段
  if (message.type === MessageType.MESSAGE && !message.to) {
    return false;
  }
  
  return true;
}

/**
 * 解析接收到的消息
 * @param {string} rawMessage - 原始消息字符串
 * @returns {object|null} 解析后的消息对象，解析失败返回 null
 */
function parseMessage(rawMessage) {
  try {
    const message = JSON.parse(rawMessage);
    return validateMessage(message) ? message : null;
  } catch (error) {
    return null;
  }
}

/**
 * 序列化消息对象
 * @param {object} message - 消息对象
 * @returns {string} JSON 字符串
 */
function serializeMessage(message) {
  return JSON.stringify(message);
}

module.exports = {
  MessageType,
  SystemMessageType,
  createMessage,
  createPingMessage,
  createPongMessage,
  createP2PMessage,
  createBroadcastMessage,
  createSystemMessage,
  validateMessage,
  parseMessage,
  serializeMessage
};
