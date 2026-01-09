"use strict";
/**
 * 任务状态管理器
 * 负责追踪所有子任务的执行状态，支持持久化和实时同步
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
exports.TaskStateManager = void 0;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const events_1 = require("../events");
/**
 * 任务状态管理器
 */
class TaskStateManager {
    tasks = new Map();
    sessionId;
    workspaceRoot;
    callbacks = [];
    autoSave;
    constructor(sessionId, workspaceRoot, autoSave = true) {
        this.sessionId = sessionId;
        this.workspaceRoot = workspaceRoot;
        this.autoSave = autoSave;
    }
    /** 创建新任务 */
    createTask(params) {
        const task = {
            id: params.id,
            parentTaskId: params.parentTaskId,
            description: params.description,
            assignedCli: params.assignedCli,
            status: 'pending',
            progress: 0,
            attempts: 0,
            maxAttempts: params.maxAttempts ?? 3,
        };
        this.tasks.set(task.id, task);
        this.notifyChange(task);
        this.autoSaveIfEnabled();
        return task;
    }
    /** 更新任务状态 */
    updateStatus(taskId, status, error) {
        const task = this.tasks.get(taskId);
        if (!task) {
            console.warn(`[TaskStateManager] 任务不存在: ${taskId}`);
            return;
        }
        task.status = status;
        if (error)
            task.error = error;
        if (status === 'running' && !task.startedAt) {
            task.startedAt = Date.now();
        }
        if (status === 'completed' || status === 'failed') {
            task.completedAt = Date.now();
        }
        if (status === 'retrying') {
            task.attempts += 1;
        }
        this.notifyChange(task);
        this.autoSaveIfEnabled();
        // 发送事件
        events_1.globalEventBus.emitEvent('task:state_changed', {
            taskId,
            data: { task, allTasks: this.getAllTasks() }
        });
    }
    /** 更新任务进度 */
    updateProgress(taskId, progress) {
        const task = this.tasks.get(taskId);
        if (!task)
            return;
        task.progress = Math.min(100, Math.max(0, progress));
        this.notifyChange(task);
    }
    /** 设置任务结果 */
    setResult(taskId, result, modifiedFiles) {
        const task = this.tasks.get(taskId);
        if (!task)
            return;
        task.result = result;
        if (modifiedFiles)
            task.modifiedFiles = modifiedFiles;
        this.autoSaveIfEnabled();
    }
    /** 获取单个任务 */
    getTask(taskId) {
        return this.tasks.get(taskId) ?? null;
    }
    /** 获取所有任务 */
    getAllTasks() {
        return Array.from(this.tasks.values());
    }
    /** 获取待执行的任务 */
    getPendingTasks(cli) {
        return this.getAllTasks().filter(t => {
            if (t.status !== 'pending')
                return false;
            if (cli && t.assignedCli !== cli)
                return false;
            return true;
        });
    }
    /** 获取指定 CLI 的任务 */
    getTasksByCli(cli) {
        return this.getAllTasks().filter(t => t.assignedCli === cli);
    }
    /** 检查是否所有任务都已完成 */
    isAllCompleted() {
        return this.getAllTasks().every(t => t.status === 'completed' || t.status === 'cancelled');
    }
    /** 检查是否有失败的任务 */
    hasFailedTasks() {
        return this.getAllTasks().some(t => t.status === 'failed');
    }
    /** 获取失败的任务 */
    getFailedTasks() {
        return this.getAllTasks().filter(t => t.status === 'failed');
    }
    /** 检查任务是否可以重试 */
    canRetry(taskId) {
        const task = this.tasks.get(taskId);
        if (!task)
            return false;
        return task.attempts < task.maxAttempts;
    }
    /** 重置任务为待执行状态（用于重试） */
    resetForRetry(taskId) {
        const task = this.tasks.get(taskId);
        if (!task)
            return;
        task.status = 'retrying';
        task.attempts += 1;
        task.error = undefined;
        task.result = undefined;
        task.progress = 0;
        this.notifyChange(task);
        this.autoSaveIfEnabled();
    }
    /** 注册状态变更回调 */
    onStateChange(callback) {
        this.callbacks.push(callback);
        return () => {
            const index = this.callbacks.indexOf(callback);
            if (index > -1)
                this.callbacks.splice(index, 1);
        };
    }
    /** 通知状态变更 */
    notifyChange(task) {
        const allTasks = this.getAllTasks();
        for (const callback of this.callbacks) {
            try {
                callback(task, allTasks);
            }
            catch (error) {
                console.error('[TaskStateManager] 回调执行失败:', error);
            }
        }
    }
    /** 自动保存（如果启用） */
    autoSaveIfEnabled() {
        if (this.autoSave) {
            this.save().catch(err => {
                console.error('[TaskStateManager] 自动保存失败:', err);
            });
        }
    }
    /** 获取存储路径 */
    getStoragePath() {
        return path.join(this.workspaceRoot, '.cli-arranger', 'tasks', `${this.sessionId}.json`);
    }
    /** 保存到文件 */
    async save() {
        const storagePath = this.getStoragePath();
        const dir = path.dirname(storagePath);
        // 确保目录存在
        if (!fs.existsSync(dir)) {
            fs.mkdirSync(dir, { recursive: true });
        }
        const data = {
            sessionId: this.sessionId,
            createdAt: Date.now(),
            updatedAt: Date.now(),
            tasks: this.getAllTasks(),
        };
        fs.writeFileSync(storagePath, JSON.stringify(data, null, 2), 'utf-8');
    }
    /** 从文件加载 */
    async load() {
        const storagePath = this.getStoragePath();
        if (!fs.existsSync(storagePath)) {
            return;
        }
        try {
            const content = fs.readFileSync(storagePath, 'utf-8');
            const data = JSON.parse(content);
            this.tasks.clear();
            for (const task of data.tasks) {
                this.tasks.set(task.id, task);
            }
        }
        catch (error) {
            console.error('[TaskStateManager] 加载失败:', error);
        }
    }
    /** 清除所有任务 */
    clear() {
        this.tasks.clear();
        this.autoSaveIfEnabled();
    }
    /** 获取统计信息 */
    getStats() {
        const tasks = this.getAllTasks();
        return {
            total: tasks.length,
            pending: tasks.filter(t => t.status === 'pending').length,
            running: tasks.filter(t => t.status === 'running' || t.status === 'retrying').length,
            completed: tasks.filter(t => t.status === 'completed').length,
            failed: tasks.filter(t => t.status === 'failed').length,
            cancelled: tasks.filter(t => t.status === 'cancelled').length,
        };
    }
}
exports.TaskStateManager = TaskStateManager;
//# sourceMappingURL=task-state-manager.js.map