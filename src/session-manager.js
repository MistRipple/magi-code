"use strict";
/**
 * Session 管理器
 * 管理 Session 生命周期、持久化
 */
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.SessionManager = void 0;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const events_1 = require("./events");
/** 生成唯一 ID */
function generateId() {
    return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}
/**
 * Session 管理器
 */
class SessionManager {
    sessions = new Map();
    currentSessionId = null;
    storageDir;
    constructor(workspaceRoot) {
        this.storageDir = path.join(workspaceRoot, '.cli-arranger', 'sessions');
        this.ensureStorageDir();
    }
    /** 确保存储目录存在 */
    ensureStorageDir() {
        if (!fs.existsSync(this.storageDir)) {
            fs.mkdirSync(this.storageDir, { recursive: true });
        }
    }
    /** 创建新 Session */
    createSession() {
        const session = {
            id: generateId(),
            createdAt: Date.now(),
            status: 'active',
            tasks: [],
            snapshots: [],
        };
        this.sessions.set(session.id, session);
        this.currentSessionId = session.id;
        this.saveSession(session);
        events_1.globalEventBus.emitEvent('session:created', { sessionId: session.id });
        return session;
    }
    /** 获取当前 Session */
    getCurrentSession() {
        if (!this.currentSessionId)
            return null;
        return this.sessions.get(this.currentSessionId) ?? null;
    }
    /** 获取或创建当前 Session */
    getOrCreateCurrentSession() {
        const current = this.getCurrentSession();
        if (current)
            return current;
        return this.createSession();
    }
    /** 切换 Session */
    switchSession(sessionId) {
        const session = this.sessions.get(sessionId);
        if (session) {
            this.currentSessionId = sessionId;
            return session;
        }
        return null;
    }
    /** 获取 Session */
    getSession(sessionId) {
        return this.sessions.get(sessionId) ?? null;
    }
    /** 获取所有 Session */
    getAllSessions() {
        return Array.from(this.sessions.values());
    }
    /** 结束 Session */
    endSession(sessionId) {
        const session = this.sessions.get(sessionId);
        if (session) {
            session.status = 'completed';
            this.saveSession(session);
            events_1.globalEventBus.emitEvent('session:ended', { sessionId });
            if (this.currentSessionId === sessionId) {
                this.currentSessionId = null;
            }
        }
    }
    /** 添加 Task 到 Session */
    addTask(sessionId, task) {
        const session = this.sessions.get(sessionId);
        if (session) {
            session.tasks.push(task);
            this.saveSession(session);
        }
    }
    /** 更新 Task */
    updateTask(sessionId, taskId, updates) {
        const session = this.sessions.get(sessionId);
        if (session) {
            const taskIndex = session.tasks.findIndex(t => t.id === taskId);
            if (taskIndex !== -1) {
                session.tasks[taskIndex] = { ...session.tasks[taskIndex], ...updates };
                this.saveSession(session);
            }
        }
    }
    /** 添加快照到 Session */
    addSnapshot(sessionId, snapshot) {
        const session = this.sessions.get(sessionId);
        if (session) {
            // 检查是否已存在该文件的快照
            const existingIndex = session.snapshots.findIndex(s => s.filePath === snapshot.filePath);
            if (existingIndex !== -1) {
                // 更新现有快照的修改信息，但保留原始内容
                session.snapshots[existingIndex].lastModifiedBy = snapshot.lastModifiedBy;
                session.snapshots[existingIndex].lastModifiedAt = snapshot.lastModifiedAt;
                session.snapshots[existingIndex].subTaskId = snapshot.subTaskId;
            }
            else {
                session.snapshots.push(snapshot);
            }
            this.saveSession(session);
        }
    }
    /** 获取文件快照 */
    getSnapshot(sessionId, filePath) {
        const session = this.sessions.get(sessionId);
        if (session) {
            return session.snapshots.find(s => s.filePath === filePath) ?? null;
        }
        return null;
    }
    /** 移除文件快照 */
    removeSnapshot(sessionId, filePath) {
        const session = this.sessions.get(sessionId);
        if (session) {
            const index = session.snapshots.findIndex(s => s.filePath === filePath);
            if (index !== -1) {
                session.snapshots.splice(index, 1);
                this.saveSession(session);
                return true;
            }
        }
        return false;
    }
    /** 保存 Session 到文件 */
    saveSession(session) {
        const filePath = path.join(this.storageDir, `${session.id}.json`);
        fs.writeFileSync(filePath, JSON.stringify(session, null, 2), 'utf-8');
    }
    /** 保存当前 Session（公开方法，供外部调用） */
    saveCurrentSession() {
        const session = this.getCurrentSession();
        if (session) {
            this.saveSession(session);
        }
    }
    /** 从文件加载 Session */
    loadSession(sessionId) {
        const filePath = path.join(this.storageDir, `${sessionId}.json`);
        if (fs.existsSync(filePath)) {
            const data = fs.readFileSync(filePath, 'utf-8');
            const session = JSON.parse(data);
            this.sessions.set(session.id, session);
            return session;
        }
        return null;
    }
    /** 加载所有 Session */
    loadAllSessions() {
        if (!fs.existsSync(this.storageDir))
            return;
        const files = fs.readdirSync(this.storageDir).filter(f => f.endsWith('.json'));
        for (const file of files) {
            const sessionId = file.replace('.json', '');
            this.loadSession(sessionId);
        }
    }
    /** 删除 Session */
    deleteSession(sessionId) {
        this.sessions.delete(sessionId);
        const filePath = path.join(this.storageDir, `${sessionId}.json`);
        if (fs.existsSync(filePath)) {
            fs.unlinkSync(filePath);
        }
        if (this.currentSessionId === sessionId) {
            this.currentSessionId = null;
        }
    }
}
exports.SessionManager = SessionManager;
//# sourceMappingURL=session-manager.js.map