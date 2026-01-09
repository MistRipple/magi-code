/**
 * Session 管理器
 * 管理 Session 生命周期、持久化
 */

import * as fs from 'fs';
import * as path from 'path';
import { Session, SessionStatus, Task, FileSnapshot } from './types';
import { globalEventBus } from './events';

/** 生成唯一 ID */
function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * Session 管理器
 */
export class SessionManager {
  private sessions: Map<string, Session> = new Map();
  private currentSessionId: string | null = null;
  private storageDir: string;

  constructor(workspaceRoot: string) {
    this.storageDir = path.join(workspaceRoot, '.cli-arranger', 'sessions');
    this.ensureStorageDir();
  }

  /** 确保存储目录存在 */
  private ensureStorageDir(): void {
    if (!fs.existsSync(this.storageDir)) {
      fs.mkdirSync(this.storageDir, { recursive: true });
    }
  }

  /** 创建新 Session */
  createSession(): Session {
    const session: Session = {
      id: generateId(),
      createdAt: Date.now(),
      status: 'active',
      tasks: [],
      snapshots: [],
    };

    this.sessions.set(session.id, session);
    this.currentSessionId = session.id;
    this.saveSession(session);

    globalEventBus.emitEvent('session:created', { sessionId: session.id });
    return session;
  }

  /** 获取当前 Session */
  getCurrentSession(): Session | null {
    if (!this.currentSessionId) return null;
    return this.sessions.get(this.currentSessionId) ?? null;
  }

  /** 获取或创建当前 Session */
  getOrCreateCurrentSession(): Session {
    const current = this.getCurrentSession();
    if (current) return current;
    return this.createSession();
  }

  /** 切换 Session */
  switchSession(sessionId: string): Session | null {
    const session = this.sessions.get(sessionId);
    if (session) {
      this.currentSessionId = sessionId;
      return session;
    }
    return null;
  }

  /** 获取 Session */
  getSession(sessionId: string): Session | null {
    return this.sessions.get(sessionId) ?? null;
  }

  /** 获取所有 Session */
  getAllSessions(): Session[] {
    return Array.from(this.sessions.values());
  }

  /** 结束 Session */
  endSession(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.status = 'completed';
      this.saveSession(session);
      globalEventBus.emitEvent('session:ended', { sessionId });

      if (this.currentSessionId === sessionId) {
        this.currentSessionId = null;
      }
    }
  }

  /** 添加 Task 到 Session */
  addTask(sessionId: string, task: Task): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.tasks.push(task);
      this.saveSession(session);
    }
  }

  /** 更新 Task */
  updateTask(sessionId: string, taskId: string, updates: Partial<Task>): void {
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
  addSnapshot(sessionId: string, snapshot: FileSnapshot): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      // 检查是否已存在该文件的快照
      const existingIndex = session.snapshots.findIndex(s => s.filePath === snapshot.filePath);
      if (existingIndex !== -1) {
        // 更新现有快照的修改信息，但保留原始内容
        session.snapshots[existingIndex].lastModifiedBy = snapshot.lastModifiedBy;
        session.snapshots[existingIndex].lastModifiedAt = snapshot.lastModifiedAt;
        session.snapshots[existingIndex].subTaskId = snapshot.subTaskId;
      } else {
        session.snapshots.push(snapshot);
      }
      this.saveSession(session);
    }
  }

  /** 获取文件快照 */
  getSnapshot(sessionId: string, filePath: string): FileSnapshot | null {
    const session = this.sessions.get(sessionId);
    if (session) {
      return session.snapshots.find(s => s.filePath === filePath) ?? null;
    }
    return null;
  }

  /** 移除文件快照 */
  removeSnapshot(sessionId: string, filePath: string): boolean {
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
  private saveSession(session: Session): void {
    const filePath = path.join(this.storageDir, `${session.id}.json`);
    fs.writeFileSync(filePath, JSON.stringify(session, null, 2), 'utf-8');
  }

  /** 保存当前 Session（公开方法，供外部调用） */
  saveCurrentSession(): void {
    const session = this.getCurrentSession();
    if (session) {
      this.saveSession(session);
    }
  }

  /** 从文件加载 Session */
  loadSession(sessionId: string): Session | null {
    const filePath = path.join(this.storageDir, `${sessionId}.json`);
    if (fs.existsSync(filePath)) {
      const data = fs.readFileSync(filePath, 'utf-8');
      const session = JSON.parse(data) as Session;
      this.sessions.set(session.id, session);
      return session;
    }
    return null;
  }

  /** 加载所有 Session */
  loadAllSessions(): void {
    if (!fs.existsSync(this.storageDir)) return;
    const files = fs.readdirSync(this.storageDir).filter(f => f.endsWith('.json'));
    for (const file of files) {
      const sessionId = file.replace('.json', '');
      this.loadSession(sessionId);
    }
  }

  /** 删除 Session */
  deleteSession(sessionId: string): void {
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

