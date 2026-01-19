import { logger, LogCategory } from '../logging';
import * as fs from 'fs';
import * as path from 'path';

export type ExecutionStateStatus = 'planned' | 'executing' | 'completed' | 'failed';

export interface ExecutionState {
  sessionId: string;
  activePlanId: string;
  taskId: string;
  status: ExecutionStateStatus;
  createdAt: number;
  updatedAt: number;
}

/**
 * 执行状态管理器
 *
 * 存储位置：.multicli/sessions/{sessionId}/execution-state.json
 * 每个会话的执行状态存储在对应会话目录下
 */
export class ExecutionStateManager {
  private workspaceRoot: string;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /** 获取会话的执行状态文件路径 */
  private getStatePath(sessionId: string): string {
    return path.join(this.workspaceRoot, '.multicli', 'sessions', sessionId, 'execution-state.json');
  }

  private ensureDir(sessionId: string): void {
    const sessionDir = path.join(this.workspaceRoot, '.multicli', 'sessions', sessionId);
    if (!fs.existsSync(sessionDir)) {
      fs.mkdirSync(sessionDir, { recursive: true });
    }
  }

  loadState(sessionId: string): ExecutionState | null {
    const filePath = this.getStatePath(sessionId);
    if (!fs.existsSync(filePath)) return null;
    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      return JSON.parse(content) as ExecutionState;
    } catch (error) {
      logger.warn('编排器.执行_状态.加载_失败', { sessionId, error }, LogCategory.ORCHESTRATOR);
      return null;
    }
  }

  saveState(state: ExecutionState): void {
    this.ensureDir(state.sessionId);
    const filePath = this.getStatePath(state.sessionId);
    fs.writeFileSync(filePath, JSON.stringify(state, null, 2), 'utf-8');
  }

  clearState(sessionId: string): void {
    const filePath = this.getStatePath(sessionId);
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
    }
  }
}
