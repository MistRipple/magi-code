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

export class ExecutionStateManager {
  private stateDir: string;

  constructor(workspaceRoot: string) {
    this.stateDir = path.join(workspaceRoot, '.multicli', 'execution-state');
    this.ensureDir();
  }

  private ensureDir(): void {
    if (!fs.existsSync(this.stateDir)) {
      fs.mkdirSync(this.stateDir, { recursive: true });
    }
  }

  loadState(sessionId: string): ExecutionState | null {
    const filePath = path.join(this.stateDir, `${sessionId}.json`);
    if (!fs.existsSync(filePath)) return null;
    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      return JSON.parse(content) as ExecutionState;
    } catch (error) {
      console.warn('[ExecutionState] 读取状态失败:', sessionId, error);
      return null;
    }
  }

  saveState(state: ExecutionState): void {
    this.ensureDir();
    const filePath = path.join(this.stateDir, `${state.sessionId}.json`);
    fs.writeFileSync(filePath, JSON.stringify(state, null, 2), 'utf-8');
  }

  clearState(sessionId: string): void {
    const filePath = path.join(this.stateDir, `${sessionId}.json`);
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
    }
  }
}
