"use strict";
/**
 * Task 管理器
 * 管理 Task 创建、状态更新、SubTask 分解
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.TaskManager = void 0;
const events_1 = require("./events");
/** 生成唯一 ID */
function generateId() {
    return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}
/**
 * Task 管理器
 */
class TaskManager {
    sessionManager;
    constructor(sessionManager) {
        this.sessionManager = sessionManager;
    }
    /** 创建新 Task */
    createTask(prompt) {
        const session = this.sessionManager.getOrCreateCurrentSession();
        const task = {
            id: generateId(),
            sessionId: session.id,
            prompt,
            status: 'pending',
            subTasks: [],
            createdAt: Date.now(),
        };
        this.sessionManager.addTask(session.id, task);
        events_1.globalEventBus.emitEvent('task:created', { sessionId: session.id, taskId: task.id });
        return task;
    }
    /** 获取 Task */
    getTask(taskId) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return null;
        return session.tasks.find(t => t.id === taskId) ?? null;
    }
    /** 更新 Task 状态 */
    updateTaskStatus(taskId, status) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return;
        const task = session.tasks.find(t => t.id === taskId);
        if (!task)
            return;
        task.status = status;
        // 更新时间戳
        if (status === 'running' && !task.startedAt) {
            task.startedAt = Date.now();
        }
        else if (status === 'completed' || status === 'failed') {
            task.completedAt = Date.now();
        }
        else if (status === 'interrupted') {
            task.interruptedAt = Date.now();
        }
        this.sessionManager.updateTask(session.id, taskId, task);
        // 发布事件
        const eventType = status === 'completed' ? 'task:completed'
            : status === 'failed' ? 'task:failed'
                : status === 'interrupted' ? 'task:interrupted'
                    : 'task:started';
        events_1.globalEventBus.emitEvent(eventType, { sessionId: session.id, taskId });
    }
    /** 添加 SubTask */
    addSubTask(taskId, description, category, assignedCli, targetFiles = []) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            throw new Error('没有活动的 Session');
        const task = session.tasks.find(t => t.id === taskId);
        if (!task)
            throw new Error(`Task 不存在: ${taskId}`);
        const subTask = {
            id: generateId(),
            taskId,
            description,
            category,
            assignedCli,
            targetFiles,
            status: 'pending',
            output: [],
        };
        task.subTasks.push(subTask);
        this.sessionManager.updateTask(session.id, taskId, task);
        return subTask;
    }
    /** 更新 SubTask 状态 */
    updateSubTaskStatus(taskId, subTaskId, status) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return;
        const task = session.tasks.find(t => t.id === taskId);
        if (!task)
            return;
        const subTask = task.subTasks.find(st => st.id === subTaskId);
        if (!subTask)
            return;
        subTask.status = status;
        if (status === 'running' && !subTask.startedAt) {
            subTask.startedAt = Date.now();
        }
        else if (status === 'completed' || status === 'failed') {
            subTask.completedAt = Date.now();
        }
        this.sessionManager.updateTask(session.id, taskId, task);
        // 检查是否所有 SubTask 都完成了
        this.checkTaskCompletion(taskId);
    }
    /** 添加 SubTask 输出 */
    addSubTaskOutput(taskId, subTaskId, output) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return;
        const task = session.tasks.find(t => t.id === taskId);
        if (!task)
            return;
        const subTask = task.subTasks.find(st => st.id === subTaskId);
        if (!subTask)
            return;
        subTask.output.push(output);
        this.sessionManager.updateTask(session.id, taskId, task);
    }
    /** 检查 Task 是否完成 */
    checkTaskCompletion(taskId) {
        const task = this.getTask(taskId);
        if (!task || task.status !== 'running')
            return;
        const allCompleted = task.subTasks.every(st => st.status === 'completed' || st.status === 'skipped');
        const anyFailed = task.subTasks.some(st => st.status === 'failed');
        if (anyFailed) {
            this.updateTaskStatus(taskId, 'failed');
        }
        else if (allCompleted) {
            this.updateTaskStatus(taskId, 'completed');
        }
    }
    /** 打断 Task */
    interruptTask(taskId) {
        this.updateTaskStatus(taskId, 'interrupted');
    }
    /** 获取当前 Session 的所有 Task */
    getAllTasks() {
        const session = this.sessionManager.getCurrentSession();
        return session?.tasks ?? [];
    }
}
exports.TaskManager = TaskManager;
//# sourceMappingURL=task-manager.js.map