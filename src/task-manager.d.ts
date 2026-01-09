/**
 * Task 管理器
 * 管理 Task 创建、状态更新、SubTask 分解
 */
import { Task, SubTask, TaskStatus, SubTaskStatus, TaskCategory, CLIType } from './types';
import { SessionManager } from './session-manager';
/**
 * Task 管理器
 */
export declare class TaskManager {
    private sessionManager;
    constructor(sessionManager: SessionManager);
    /** 创建新 Task */
    createTask(prompt: string): Task;
    /** 获取 Task */
    getTask(taskId: string): Task | null;
    /** 更新 Task 状态 */
    updateTaskStatus(taskId: string, status: TaskStatus): void;
    /** 添加 SubTask */
    addSubTask(taskId: string, description: string, category: TaskCategory, assignedCli: CLIType, targetFiles?: string[]): SubTask;
    /** 更新 SubTask 状态 */
    updateSubTaskStatus(taskId: string, subTaskId: string, status: SubTaskStatus): void;
    /** 添加 SubTask 输出 */
    addSubTaskOutput(taskId: string, subTaskId: string, output: string): void;
    /** 检查 Task 是否完成 */
    private checkTaskCompletion;
    /** 打断 Task */
    interruptTask(taskId: string): void;
    /** 获取当前 Session 的所有 Task */
    getAllTasks(): Task[];
}
//# sourceMappingURL=task-manager.d.ts.map