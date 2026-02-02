"use strict";
/**
 * WebSocket 实时消息推送系统 - 核心接口定义
 *
 * @description 本文件定义了服务端和客户端的所有核心接口、类型和枚举
 * @version 1.0.0
 * @author Antigravity (Google Deepmind)
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.PROTOCOL_VERSION = exports.ErrorCodes = exports.SystemEventType = exports.ServerMessageType = exports.ClientMessageType = void 0;
// ============================================================================
// 消息协议类型定义
// ============================================================================
/**
 * 客户端消息类型枚举
 */
var ClientMessageType;
(function (ClientMessageType) {
    // 连接控制
    ClientMessageType["PING"] = "ping";
    ClientMessageType["AUTH"] = "auth";
    // 房间操作
    ClientMessageType["JOIN_ROOM"] = "join_room";
    ClientMessageType["LEAVE_ROOM"] = "leave_room";
    ClientMessageType["CREATE_ROOM"] = "create_room";
    // 消息发送
    ClientMessageType["SEND_MESSAGE"] = "send_message";
    ClientMessageType["BROADCAST"] = "broadcast";
    // 查询操作
    ClientMessageType["GET_ROOM_MEMBERS"] = "get_room_members";
    ClientMessageType["GET_USER_ROOMS"] = "get_user_rooms";
    // 自定义
    ClientMessageType["CUSTOM"] = "custom";
})(ClientMessageType || (exports.ClientMessageType = ClientMessageType = {}));
/**
 * 服务端消息类型枚举
 */
var ServerMessageType;
(function (ServerMessageType) {
    // 连接控制
    ServerMessageType["PONG"] = "pong";
    ServerMessageType["AUTH_SUCCESS"] = "auth_success";
    ServerMessageType["AUTH_FAILURE"] = "auth_failure";
    // 房间通知
    ServerMessageType["ROOM_JOINED"] = "room_joined";
    ServerMessageType["ROOM_LEFT"] = "room_left";
    ServerMessageType["ROOM_CREATED"] = "room_created";
    ServerMessageType["USER_JOINED"] = "user_joined";
    ServerMessageType["USER_LEFT"] = "user_left";
    // 消息接收
    ServerMessageType["MESSAGE"] = "message";
    ServerMessageType["BROADCAST"] = "broadcast";
    // 查询响应
    ServerMessageType["ROOM_MEMBERS"] = "room_members";
    ServerMessageType["USER_ROOMS"] = "user_rooms";
    // 错误和通知
    ServerMessageType["ERROR"] = "error";
    ServerMessageType["NOTIFICATION"] = "notification";
    // 自定义
    ServerMessageType["CUSTOM"] = "custom";
})(ServerMessageType || (exports.ServerMessageType = ServerMessageType = {}));
// ============================================================================
// 事件分发相关类型定义
// ============================================================================
/**
 * 系统事件类型枚举
 */
var SystemEventType;
(function (SystemEventType) {
    // 连接事件
    SystemEventType["CONNECTION_OPENED"] = "connection:opened";
    SystemEventType["CONNECTION_CLOSED"] = "connection:closed";
    SystemEventType["CONNECTION_ERROR"] = "connection:error";
    SystemEventType["HEARTBEAT_TIMEOUT"] = "heartbeat:timeout";
    // 消息事件
    SystemEventType["MESSAGE_RECEIVED"] = "message:received";
    SystemEventType["MESSAGE_SENT"] = "message:sent";
    SystemEventType["MESSAGE_ERROR"] = "message:error";
    // 房间事件
    SystemEventType["ROOM_CREATED"] = "room:created";
    SystemEventType["ROOM_DELETED"] = "room:deleted";
    SystemEventType["USER_JOINED_ROOM"] = "user:joined_room";
    SystemEventType["USER_LEFT_ROOM"] = "user:left_room";
    // 系统事件
    SystemEventType["SERVER_STARTED"] = "server:started";
    SystemEventType["SERVER_STOPPED"] = "server:stopped";
    SystemEventType["SERVER_ERROR"] = "server:error";
})(SystemEventType || (exports.SystemEventType = SystemEventType = {}));
// ============================================================================
// 错误码常量定义
// ============================================================================
exports.ErrorCodes = {
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
};
// ============================================================================
// 协议版本常量
// ============================================================================
exports.PROTOCOL_VERSION = '1.0.0';
//# sourceMappingURL=websocket-types.js.map