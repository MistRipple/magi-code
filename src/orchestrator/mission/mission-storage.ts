/**
 * Mission Storage - Mission 持久化存储
 *
 * 负责 Mission 及其关联数据的存储和加载
 */

import * as fs from 'fs';
import * as path from 'path';
import { EventEmitter } from 'events';
import type { UnifiedTodo } from '../../todo/types';
import {
  Mission,
  Contract,
  Assignment,
  MissionStatus,
  CreateMissionParams,
} from './types';

/**
 * Mission 存储接口
 */
export interface IMissionStorage {
  // Mission 操作
  save(mission: Mission): Promise<void>;
  load(id: string): Promise<Mission | null>;
  update(mission: Mission): Promise<void>;
  delete(id: string): Promise<void>;
  listBySession(sessionId: string): Promise<Mission[]>;

  // 查询操作
  findByStatus(status: MissionStatus): Promise<Mission[]>;
  getLatestBySession(sessionId: string): Promise<Mission | null>;
}

/**
 * 内存实现的 Mission 存储
 * 用于开发和测试，生产环境应使用持久化实现
 */
export class InMemoryMissionStorage implements IMissionStorage {
  private missions: Map<string, Mission> = new Map();
  private sessionIndex: Map<string, Set<string>> = new Map();

  async save(mission: Mission): Promise<void> {
    this.missions.set(mission.id, { ...mission });

    // 更新 session 索引
    if (!this.sessionIndex.has(mission.sessionId)) {
      this.sessionIndex.set(mission.sessionId, new Set());
    }
    this.sessionIndex.get(mission.sessionId)!.add(mission.id);
  }

  async load(id: string): Promise<Mission | null> {
    const mission = this.missions.get(id);
    return mission ? { ...mission } : null;
  }

  async update(mission: Mission): Promise<void> {
    if (!this.missions.has(mission.id)) {
      throw new Error(`Mission not found: ${mission.id}`);
    }
    mission.updatedAt = Date.now();
    this.missions.set(mission.id, { ...mission });
  }

  async delete(id: string): Promise<void> {
    const mission = this.missions.get(id);
    if (mission) {
      this.missions.delete(id);
      this.sessionIndex.get(mission.sessionId)?.delete(id);
    }
  }

  async listBySession(sessionId: string): Promise<Mission[]> {
    const missionIds = this.sessionIndex.get(sessionId);
    if (!missionIds) return [];

    return Array.from(missionIds)
      .map(id => this.missions.get(id)!)
      .filter(Boolean)
      .sort((a, b) => a.createdAt - b.createdAt);
  }

  async findByStatus(status: MissionStatus): Promise<Mission[]> {
    return Array.from(this.missions.values())
      .filter(m => m.status === status)
      .sort((a, b) => a.createdAt - b.createdAt);
  }

  async getLatestBySession(sessionId: string): Promise<Mission | null> {
    const missions = await this.listBySession(sessionId);
    return missions[missions.length - 1] || null;
  }

  // 辅助方法：清空所有数据（用于测试）
  clear(): void {
    this.missions.clear();
    this.sessionIndex.clear();
  }
}

/**
 * Mission 存储管理器
 * 提供统一的存储访问接口，支持事件通知
 */
export class MissionStorageManager extends EventEmitter {
  private storage: IMissionStorage;

  constructor(storage?: IMissionStorage) {
    super();
    this.storage = storage || new InMemoryMissionStorage();
  }

  /**
   * 创建新 Mission
   */
  async createMission(params: CreateMissionParams): Promise<Mission> {
    const now = Date.now();
    const mission: Mission = {
      id: `mission_${now}_${Math.random().toString(36).substring(2, 11)}`,
      sessionId: params.sessionId,
      userPrompt: params.userPrompt,
      goal: '',
      analysis: '',
      context: params.context || '',
      constraints: [],
      acceptanceCriteria: [],
      contracts: [],
      assignments: [],
      riskLevel: 'medium',
      riskFactors: [],
      executionPath: 'standard',
      status: 'draft',
      phase: 'goal_understanding',
      createdAt: now,
      updatedAt: now,
    };

    await this.storage.save(mission);
    this.emit('missionCreated', { mission });
    return mission;
  }

  /**
   * 保存 Mission
   */
  async save(mission: Mission): Promise<void> {
    await this.storage.save(mission);
  }

  /**
   * 加载 Mission
   */
  async load(id: string): Promise<Mission | null> {
    return this.storage.load(id);
  }

  /**
   * 更新 Mission
   */
  async update(mission: Mission): Promise<void> {
    const oldMission = await this.storage.load(mission.id);
    await this.storage.update(mission);

    if (oldMission && oldMission.status !== mission.status) {
      this.emit('missionStatusChanged', {
        mission,
        missionId: mission.id,
        oldStatus: oldMission.status,
        newStatus: mission.status,
      });
    }

    if (oldMission && oldMission.phase !== mission.phase) {
      this.emit('missionPhaseChanged', {
        mission,
        missionId: mission.id,
        oldPhase: oldMission.phase,
        newPhase: mission.phase,
      });
    }
  }

  /**
   * 删除 Mission
   */
  async delete(id: string): Promise<void> {
    await this.storage.delete(id);
    this.emit('missionDeleted', { missionId: id });
  }

  /**
   * 列出会话的所有 Mission
   */
  async listBySession(sessionId: string): Promise<Mission[]> {
    return this.storage.listBySession(sessionId);
  }

  /**
   * 按状态查找 Mission
   */
  async findByStatus(status: MissionStatus): Promise<Mission[]> {
    return this.storage.findByStatus(status);
  }

  /**
   * 获取会话最新的 Mission
   */
  async getLatestBySession(sessionId: string): Promise<Mission | null> {
    return this.storage.getLatestBySession(sessionId);
  }

  /**
   * 更新 Mission 中的 Assignment
   */
  async updateAssignment(missionId: string, assignment: Assignment): Promise<void> {
    const mission = await this.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    const index = mission.assignments.findIndex(a => a.id === assignment.id);
    if (index === -1) {
      mission.assignments.push(assignment);
    } else {
      mission.assignments[index] = assignment;
    }

    await this.update(mission);
    this.emit('assignmentUpdated', { missionId, assignment });
  }

  /**
   * 更新 Assignment 中的 Todo
   */
  async updateTodo(missionId: string, assignmentId: string, todo: UnifiedTodo): Promise<void> {
    const mission = await this.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    const assignment = mission.assignments.find(a => a.id === assignmentId);
    if (!assignment) {
      throw new Error(`Assignment not found: ${assignmentId}`);
    }

    const todoIndex = assignment.todos.findIndex(t => t.id === todo.id);
    if (todoIndex === -1) {
      assignment.todos.push(todo);
    } else {
      assignment.todos[todoIndex] = todo;
    }

    // 更新 Assignment 进度
    this.calculateAssignmentProgress(assignment);

    await this.update(mission);
    this.emit('todoUpdated', { missionId, assignmentId, todo });
  }

  /**
   * 更新 Mission 中的 Contract
   */
  async updateContract(missionId: string, contract: Contract): Promise<void> {
    const mission = await this.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    const index = mission.contracts.findIndex(c => c.id === contract.id);
    if (index === -1) {
      mission.contracts.push(contract);
    } else {
      mission.contracts[index] = contract;
    }

    await this.update(mission);
    this.emit('contractUpdated', { missionId, contract });
  }

  /**
   * 计算 Assignment 进度
   */
  private calculateAssignmentProgress(assignment: Assignment): void {
    if (assignment.todos.length === 0) {
      assignment.progress = 0;
      return;
    }

    const completedCount = assignment.todos.filter(
      t => t.status === 'completed' || t.status === 'skipped'
    ).length;

    assignment.progress = Math.round((completedCount / assignment.todos.length) * 100);
  }
}

/**
 * 文件系统 Mission 存储实现
 * 将 Mission 持久化到文件系统，按 session 目录存储
 *
 * 目录结构：
 * .magi/sessions/{sessionId}/missions/{missionId}.json
 */
export class FileBasedMissionStorage implements IMissionStorage {
  private sessionsDir: string;
  private missions: Map<string, Mission> = new Map();
  private sessionIndex: Map<string, Set<string>> = new Map();
  private loaded = false;

  constructor(sessionsDir: string) {
    this.sessionsDir = sessionsDir;
  }

  private getSessionMissionsDir(sessionId: string): string {
    return path.join(this.sessionsDir, sessionId, 'missions');
  }

  private async ensureSessionMissionsDir(sessionId: string): Promise<void> {
    const dir = this.getSessionMissionsDir(sessionId);
    await fs.promises.mkdir(dir, { recursive: true });
  }

  private getMissionFilePath(mission: Mission): string;
  private getMissionFilePath(missionId: string, sessionId: string): string;
  private getMissionFilePath(missionOrId: Mission | string, sessionId?: string): string {
    if (typeof missionOrId === 'string') {
      return path.join(this.getSessionMissionsDir(sessionId!), `${missionOrId}.json`);
    }
    return path.join(this.getSessionMissionsDir(missionOrId.sessionId), `${missionOrId.id}.json`);
  }

  private async ensureLoaded(): Promise<void> {
    if (this.loaded) return;

    try {
      await fs.promises.access(this.sessionsDir);
    } catch {
      this.loaded = true;
      return;
    }

    const sessionEntries = await fs.promises.readdir(this.sessionsDir, { withFileTypes: true });
    for (const entry of sessionEntries) {
      if (!entry.isDirectory()) continue;

      const missionsDir = path.join(this.sessionsDir, entry.name, 'missions');
      try {
        await fs.promises.access(missionsDir);
      } catch {
        continue;
      }

      const files = await fs.promises.readdir(missionsDir);
      for (const file of files) {
        if (!file.endsWith('.json')) continue;

        const filePath = path.join(missionsDir, file);
        try {
          const content = await fs.promises.readFile(filePath, 'utf-8');
          const mission: Mission = JSON.parse(content);
          this.missions.set(mission.id, mission);

          if (!this.sessionIndex.has(mission.sessionId)) {
            this.sessionIndex.set(mission.sessionId, new Set());
          }
          this.sessionIndex.get(mission.sessionId)!.add(mission.id);
        } catch {
          // 跳过无效文件
        }
      }
    }

    this.loaded = true;
  }

  private async saveToDisk(mission: Mission): Promise<void> {
    await this.ensureSessionMissionsDir(mission.sessionId);
    const filePath = this.getMissionFilePath(mission);
    await fs.promises.writeFile(filePath, JSON.stringify(mission, null, 2), 'utf-8');
  }

  private async deleteFromDisk(mission: Mission): Promise<void> {
    const filePath = this.getMissionFilePath(mission);
    try {
      await fs.promises.unlink(filePath);
    } catch (err: unknown) {
      if ((err as NodeJS.ErrnoException).code !== 'ENOENT') {
        throw err;
      }
    }
  }

  async save(mission: Mission): Promise<void> {
    await this.ensureLoaded();
    this.missions.set(mission.id, { ...mission });

    if (!this.sessionIndex.has(mission.sessionId)) {
      this.sessionIndex.set(mission.sessionId, new Set());
    }
    this.sessionIndex.get(mission.sessionId)!.add(mission.id);

    await this.saveToDisk(mission);
  }

  async load(id: string): Promise<Mission | null> {
    await this.ensureLoaded();
    const mission = this.missions.get(id);
    return mission ? { ...mission } : null;
  }

  async update(mission: Mission): Promise<void> {
    await this.ensureLoaded();
    if (!this.missions.has(mission.id)) {
      throw new Error(`Mission not found: ${mission.id}`);
    }
    mission.updatedAt = Date.now();
    this.missions.set(mission.id, { ...mission });
    await this.saveToDisk(mission);
  }

  async delete(id: string): Promise<void> {
    await this.ensureLoaded();
    const mission = this.missions.get(id);
    if (mission) {
      this.missions.delete(id);
      this.sessionIndex.get(mission.sessionId)?.delete(id);
      await this.deleteFromDisk(mission);
    }
  }

  async listBySession(sessionId: string): Promise<Mission[]> {
    await this.ensureLoaded();
    const missionIds = this.sessionIndex.get(sessionId);
    if (!missionIds) return [];

    return Array.from(missionIds)
      .map(id => this.missions.get(id)!)
      .filter(Boolean)
      .sort((a, b) => a.createdAt - b.createdAt);
  }

  async findByStatus(status: MissionStatus): Promise<Mission[]> {
    await this.ensureLoaded();
    return Array.from(this.missions.values())
      .filter(m => m.status === status)
      .sort((a, b) => a.createdAt - b.createdAt);
  }

  async getLatestBySession(sessionId: string): Promise<Mission | null> {
    const missions = await this.listBySession(sessionId);
    return missions[missions.length - 1] || null;
  }
}

/**
 * 创建默认的 MissionStorage 实例（内存版）
 */
export function createMissionStorage(): MissionStorageManager {
  return new MissionStorageManager(new InMemoryMissionStorage());
}

/**
 * 创建文件系统 MissionStorage 实例
 * @param sessionsDir sessions 基础目录（.magi/sessions）
 */
export function createFileBasedMissionStorage(sessionsDir: string): MissionStorageManager {
  return new MissionStorageManager(new FileBasedMissionStorage(sessionsDir));
}
