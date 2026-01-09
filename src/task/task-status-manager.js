"use strict";
/**
 * 任务状态管理器
 * 管理任务状态的实时更新和通知
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.TaskStatusManager = void 0;
const events_1 = require("events");
/**
 * 任务状态管理器类
 */
class TaskStatusManager extends events_1.EventEmitter {
    taskProgress = new Map();
    subTaskProgress = new Map();
    updateInterval = null;
    constructor() {
        super();
    }
    /** 开始跟踪任务 */
    startTask(taskId, totalSubTasks) {
        const progress = {
            taskId,
            status: 'running',
            progress: 0,
            completedSubTasks: 0,
            totalSubTasks,
            startTime: Date.now(),
            elapsedTime: 0,
        };
        this.taskProgress.set(taskId, progress);
        this.emitUpdate({ type: 'task', taskId, status: 'running', progress: 0, timestamp: Date.now() });
        this.startProgressUpdates();
    }
    /** 开始跟踪子任务 */
    startSubTask(taskId, subTaskId, cli, description) {
        const progress = {
            subTaskId,
            taskId,
            status: 'running',
            cli,
            description,
            startTime: Date.now(),
            output: [],
        };
        this.subTaskProgress.set(subTaskId, progress);
        const taskProg = this.taskProgress.get(taskId);
        if (taskProg) {
            taskProg.currentSubTask = subTaskId;
        }
        this.emitUpdate({ type: 'subtask', taskId, subTaskId, status: 'running', timestamp: Date.now() });
    }
    /** 添加子任务输出 */
    addSubTaskOutput(subTaskId, output) {
        const progress = this.subTaskProgress.get(subTaskId);
        if (progress) {
            progress.output.push(output);
            this.emit('output', { subTaskId, output, timestamp: Date.now() });
        }
    }
    /** 完成子任务 */
    completeSubTask(subTaskId, success) {
        const progress = this.subTaskProgress.get(subTaskId);
        if (progress) {
            progress.status = success ? 'completed' : 'failed';
            progress.endTime = Date.now();
            const taskProg = this.taskProgress.get(progress.taskId);
            if (taskProg) {
                taskProg.completedSubTasks++;
                taskProg.progress = Math.round((taskProg.completedSubTasks / taskProg.totalSubTasks) * 100);
                if (taskProg.currentSubTask === subTaskId) {
                    taskProg.currentSubTask = undefined;
                }
            }
            this.emitUpdate({
                type: 'subtask',
                taskId: progress.taskId,
                subTaskId,
                status: progress.status,
                timestamp: Date.now(),
            });
        }
    }
    /** 完成任务 */
    completeTask(taskId, status) {
        const progress = this.taskProgress.get(taskId);
        if (progress) {
            progress.status = status;
            progress.progress = status === 'completed' ? 100 : progress.progress;
            progress.elapsedTime = Date.now() - progress.startTime;
        }
        this.emitUpdate({ type: 'task', taskId, status, progress: progress?.progress, timestamp: Date.now() });
        this.stopProgressUpdates();
    }
    /** 获取任务进度 */
    getTaskProgress(taskId) {
        return this.taskProgress.get(taskId);
    }
    /** 获取子任务进度 */
    getSubTaskProgress(subTaskId) {
        return this.subTaskProgress.get(subTaskId);
    }
    /** 获取所有活动任务 */
    getActiveTasks() {
        return Array.from(this.taskProgress.values()).filter(p => p.status === 'running');
    }
    /** 清理任务数据 */
    clearTask(taskId) {
        this.taskProgress.delete(taskId);
        for (const [id, prog] of this.subTaskProgress) {
            if (prog.taskId === taskId)
                this.subTaskProgress.delete(id);
        }
    }
    emitUpdate(update) {
        this.emit('statusUpdate', update);
    }
    startProgressUpdates() {
        if (this.updateInterval)
            return;
        this.updateInterval = setInterval(() => {
            for (const progress of this.taskProgress.values()) {
                if (progress.status === 'running') {
                    progress.elapsedTime = Date.now() - progress.startTime;
                    this.emit('progressTick', { taskId: progress.taskId, elapsed: progress.elapsedTime });
                }
            }
        }, 1000);
    }
    stopProgressUpdates() {
        if (this.updateInterval) {
            clearInterval(this.updateInterval);
            this.updateInterval = null;
        }
    }
    dispose() {
        this.stopProgressUpdates();
        this.taskProgress.clear();
        this.subTaskProgress.clear();
        this.removeAllListeners();
    }
}
exports.TaskStatusManager = TaskStatusManager;
//# sourceMappingURL=task-status-manager.js.map