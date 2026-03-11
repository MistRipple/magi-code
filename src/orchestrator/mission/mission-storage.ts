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
  MissionPhase,
  MissionDeliveryStatus,
  MissionContinuationPolicy,
  CreateMissionParams,
  Constraint,
  AcceptanceCriterion,
  RiskLevel,
  ExecutionPath,
} from './types';
import { normalizeAssignments, generateEntityId } from './data-normalizer';
import { logger, LogCategory } from '../../logging';

function normalizeMissionTextItems(items: string[] | undefined): string[] {
  if (!Array.isArray(items)) {
    return [];
  }
  return Array.from(new Set(
    items
      .map(item => (typeof item === 'string' ? item.trim() : ''))
      .filter(item => item.length > 0)
  ));
}

function buildMissionConstraints(items: string[] | undefined): Constraint[] {
  return normalizeMissionTextItems(items).map((description) => ({
    id: generateEntityId('constraint'),
    type: 'must',
    description,
    source: 'user',
  }));
}

function buildMissionAcceptanceCriteria(items: string[] | undefined): AcceptanceCriterion[] {
  return normalizeMissionTextItems(items).map((description) => ({
    id: generateEntityId('acceptance'),
    description,
    verifiable: false,
    status: 'pending',
  }));
}

function normalizeMissionRiskLevel(level: RiskLevel | undefined): RiskLevel {
  return level === 'low' || level === 'medium' || level === 'high'
    ? level
    : 'medium';
}

function normalizeMissionExecutionPath(pathValue: ExecutionPath | undefined): ExecutionPath {
  return pathValue === 'light' || pathValue === 'standard' || pathValue === 'full'
    ? pathValue
    : 'standard';
}

function normalizeMissionDeliveryStatus(status: MissionDeliveryStatus | undefined): MissionDeliveryStatus {
  if (status === 'pending' || status === 'passed' || status === 'failed' || status === 'blocked' || status === 'skipped') {
    return status;
  }
  return 'pending';
}

function normalizeMissionContinuationPolicy(policy: MissionContinuationPolicy | undefined): MissionContinuationPolicy {
  return policy === 'auto' || policy === 'ask' || policy === 'stop'
    ? policy
    : 'auto';
}

function normalizeMissionDeliveryWarnings(warnings: string[] | undefined): string[] {
  return normalizeMissionTextItems(warnings);
}

const TERMINAL_MISSION_STATUSES = new Set<MissionStatus>(['completed', 'failed', 'cancelled']);

const MISSION_STATUS_TO_PHASE: Record<MissionStatus, MissionPhase> = {
  draft: 'goal_understanding',
  planning: 'worker_planning',
  pending_review: 'plan_review',
  pending_approval: 'plan_review',
  executing: 'execution',
  paused: 'execution',
  reviewing: 'verification',
  completed: 'summary',
  failed: 'summary',
  cancelled: 'summary',
};

function deriveMissionPhase(status: MissionStatus): MissionPhase {
  const phase = MISSION_STATUS_TO_PHASE[status];
  if (!phase) {
    throw new Error(`Invalid mission status for phase derivation: ${String(status)}`);
  }
  return phase;
}

function normalizeMissionPhase(mission: Mission): Mission {
  const phase = deriveMissionPhase(mission.status);
  return mission.phase === phase ? mission : { ...mission, phase };
}

function normalizeMissionDelivery(mission: Mission): Mission {
  const deliveryStatus = normalizeMissionDeliveryStatus(mission.deliveryStatus);
  const continuationPolicy = normalizeMissionContinuationPolicy(mission.continuationPolicy);
  const deliveryWarnings = normalizeMissionDeliveryWarnings(mission.deliveryWarnings);
  const deliveryUpdatedAt = Number.isFinite(mission.deliveryUpdatedAt || 0)
    ? mission.deliveryUpdatedAt
    : (deliveryStatus !== 'pending' ? mission.updatedAt : undefined);
  return {
    ...mission,
    deliveryStatus,
    deliveryWarnings,
    deliveryUpdatedAt,
    continuationPolicy,
  };
}

const ALLOWED_MISSION_STATUS_TRANSITIONS: Record<MissionStatus, MissionStatus[]> = {
  draft: ['planning', 'pending_review', 'pending_approval', 'executing', 'cancelled'],
  planning: ['pending_review', 'pending_approval', 'executing', 'failed', 'cancelled'],
  pending_review: ['planning', 'pending_approval', 'executing', 'failed', 'cancelled'],
  pending_approval: ['planning', 'executing', 'cancelled'],
  executing: ['paused', 'reviewing', 'completed', 'failed', 'cancelled'],
  paused: ['executing', 'failed', 'cancelled'],
  reviewing: ['executing', 'completed', 'failed', 'cancelled'],
  completed: [],
  failed: [],
  cancelled: [],
};

interface TransitionMissionStatusOptions {
  failureReason?: string;
}

function applyMissionStatusTransition(
  mission: Mission,
  nextStatus: MissionStatus,
  options: TransitionMissionStatusOptions,
  now: number,
): Mission {
  const resolveDeliveryStatus = (): MissionDeliveryStatus => {
    if (mission.deliveryStatus && mission.deliveryStatus !== 'pending') {
      return mission.deliveryStatus;
    }
    if (nextStatus === 'failed') {
      return 'blocked';
    }
    if (nextStatus === 'cancelled') {
      return 'skipped';
    }
    return mission.deliveryStatus;
  };

  if (nextStatus === 'failed') {
    const failureReason = options.failureReason?.trim();
    if (!failureReason) {
      throw new Error('Mission failureReason is required when transitioning to failed');
    }
    return normalizeMissionPhase({
      ...mission,
      status: nextStatus,
      failureReason,
      deliveryStatus: resolveDeliveryStatus(),
    });
  }

  if (nextStatus === 'executing') {
    return normalizeMissionPhase({
      ...mission,
      status: nextStatus,
      failureReason: undefined,
      startedAt: mission.startedAt ?? now,
      deliveryStatus: resolveDeliveryStatus(),
    });
  }

  if (nextStatus === 'completed') {
    return normalizeMissionPhase({
      ...mission,
      status: nextStatus,
      failureReason: undefined,
      completedAt: now,
      deliveryStatus: resolveDeliveryStatus(),
    });
  }

  return normalizeMissionPhase({
    ...mission,
    status: nextStatus,
    failureReason: undefined,
    deliveryStatus: resolveDeliveryStatus(),
  });
}

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

  private normalizeMission(mission: Mission): Mission {
    return normalizeMissionPhase(normalizeMissionDelivery({
      ...mission,
      assignments: normalizeAssignments(mission.assignments),
    }));
  }

  async save(mission: Mission): Promise<void> {
    this.missions.set(mission.id, this.normalizeMission(mission));

    // 更新 session 索引
    if (!this.sessionIndex.has(mission.sessionId)) {
      this.sessionIndex.set(mission.sessionId, new Set());
    }
    this.sessionIndex.get(mission.sessionId)!.add(mission.id);
  }

  async load(id: string): Promise<Mission | null> {
    const mission = this.missions.get(id);
    return mission ? this.normalizeMission(mission) : null;
  }

  async update(mission: Mission): Promise<void> {
    if (!this.missions.has(mission.id)) {
      throw new Error(`Mission not found: ${mission.id}`);
    }
    mission.updatedAt = Date.now();
    this.missions.set(mission.id, this.normalizeMission(mission));
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
    const constraints = buildMissionConstraints(params.constraints);
    const acceptanceCriteria = buildMissionAcceptanceCriteria(params.acceptanceCriteria);
    const riskLevel = normalizeMissionRiskLevel(params.riskLevel);
    const executionPath = normalizeMissionExecutionPath(params.executionPath);
    const riskFactors = normalizeMissionTextItems(params.riskFactors);
    const mission: Mission = {
      id: `mission_${now}_${Math.random().toString(36).substring(2, 11)}`,
      sessionId: params.sessionId,
      userPrompt: params.userPrompt,
      goal: params.goal?.trim() || '',
      analysis: params.analysis?.trim() || '',
      context: params.context || '',
      constraints,
      acceptanceCriteria,
      contracts: [],
      assignments: [],
      riskLevel,
      riskFactors,
      executionPath,
      status: 'draft',
      phase: deriveMissionPhase('draft'),
      deliveryStatus: 'pending',
      continuationPolicy: normalizeMissionContinuationPolicy(params.continuationPolicy),
      createdAt: now,
      updatedAt: now,
    };

    const normalizedMission = normalizeMissionPhase(mission);
    await this.storage.save(normalizedMission);
    this.emit('missionCreated', { mission: normalizedMission });
    return normalizedMission;
  }

  /**
   * 保存 Mission
   */
  async save(mission: Mission): Promise<void> {
    await this.storage.save(normalizeMissionPhase(normalizeMissionDelivery(mission)));
  }

  /**
   * 加载 Mission
   */
  async load(id: string): Promise<Mission | null> {
    return this.storage.load(id);
  }

  async transitionStatus(
    missionId: string,
    nextStatus: MissionStatus,
    options: TransitionMissionStatusOptions = {},
  ): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    if (mission.status === nextStatus) {
      const normalizedMission = normalizeMissionPhase(mission);
      if (normalizedMission.phase !== mission.phase) {
        await this.persistMission(normalizedMission, mission);
      }
      return normalizedMission;
    }

    if (TERMINAL_MISSION_STATUSES.has(mission.status)) {
      throw new Error(`Mission terminal status is sticky: ${mission.status} -> ${nextStatus}`);
    }

    const allowedTransitions = ALLOWED_MISSION_STATUS_TRANSITIONS[mission.status];
    if (!allowedTransitions) {
      throw new Error(`Invalid mission status value: ${String(mission.status)}`);
    }

    if (!allowedTransitions.includes(nextStatus)) {
      throw new Error(`Invalid mission status transition: ${mission.status} -> ${nextStatus}`);
    }

    const transitionedMission = applyMissionStatusTransition(mission, nextStatus, options, Date.now());
    await this.persistMission(transitionedMission, mission);
    return transitionedMission;
  }

  async updateDelivery(
    missionId: string,
    input: {
      status: MissionDeliveryStatus;
      summary?: string;
      details?: string;
      warnings?: string[];
      updatedAt?: number;
      continuationPolicy?: MissionContinuationPolicy;
      continuationReason?: string;
    },
  ): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    const now = Date.now();
    const summary = typeof input.summary === 'string' && input.summary.trim().length > 0
      ? input.summary.trim()
      : mission.deliverySummary;
    const details = typeof input.details === 'string' && input.details.trim().length > 0
      ? input.details.trim()
      : mission.deliveryDetails;
    const warnings = Array.isArray(input.warnings)
      ? normalizeMissionDeliveryWarnings(input.warnings)
      : mission.deliveryWarnings;
    const continuationPolicy = normalizeMissionContinuationPolicy(
      input.continuationPolicy ?? mission.continuationPolicy
    );
    const continuationReason = typeof input.continuationReason === 'string' && input.continuationReason.trim().length > 0
      ? input.continuationReason.trim()
      : mission.continuationReason;

    const nextMission = normalizeMissionPhase(normalizeMissionDelivery({
      ...mission,
      deliveryStatus: normalizeMissionDeliveryStatus(input.status),
      deliverySummary: summary,
      deliveryDetails: details,
      deliveryWarnings: warnings,
      deliveryUpdatedAt: input.updatedAt ?? now,
      continuationPolicy,
      continuationReason,
    }));

    await this.persistMission(nextMission, mission);
    return nextMission;
  }

  /**
   * 更新 Mission
   */
  async update(mission: Mission): Promise<void> {
    const oldMission = await this.storage.load(mission.id);
    if (oldMission && oldMission.status !== mission.status) {
      throw new Error(
        `Mission status must transition via transitionStatus: ${oldMission.status} -> ${mission.status}`
      );
    }
    if (oldMission && oldMission.phase !== mission.phase) {
      throw new Error(
        `Mission phase is derived from status and cannot be updated directly: ${oldMission.phase} -> ${mission.phase}`
      );
    }
    await this.persistMission(mission, oldMission);
  }

  private async persistMission(mission: Mission, oldMission?: Mission | null): Promise<void> {
    const normalizedMission = normalizeMissionPhase(normalizeMissionDelivery(mission));
    const previousMission = typeof oldMission === 'undefined'
      ? await this.storage.load(mission.id)
      : oldMission;
    await this.storage.update(normalizedMission);

    if (previousMission && previousMission.status !== normalizedMission.status) {
      this.emit('missionStatusChanged', {
        mission: normalizedMission,
        missionId: normalizedMission.id,
        oldStatus: previousMission.status,
        newStatus: normalizedMission.status,
      });
    }

    if (previousMission && previousMission.phase !== normalizedMission.phase) {
      this.emit('missionPhaseChanged', {
        mission: normalizedMission,
        missionId: normalizedMission.id,
        oldPhase: previousMission.phase,
        newPhase: normalizedMission.phase,
      });
    }

    if (previousMission) {
      const deliveryChanged = previousMission.deliveryStatus !== normalizedMission.deliveryStatus
        || previousMission.deliverySummary !== normalizedMission.deliverySummary
        || previousMission.deliveryDetails !== normalizedMission.deliveryDetails
        || (previousMission.deliveryWarnings || []).join('||') !== (normalizedMission.deliveryWarnings || []).join('||')
        || previousMission.continuationPolicy !== normalizedMission.continuationPolicy
        || previousMission.continuationReason !== normalizedMission.continuationReason;
      if (deliveryChanged) {
        this.emit('missionDeliveryChanged', {
          mission: normalizedMission,
          missionId: normalizedMission.id,
          oldStatus: previousMission.deliveryStatus,
          newStatus: normalizedMission.deliveryStatus,
        });
      }
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

  private normalizeMission(mission: Mission): Mission {
    return normalizeMissionPhase(normalizeMissionDelivery({
      ...mission,
      assignments: normalizeAssignments(mission.assignments),
    }));
  }

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
          const mission = this.normalizeMission(JSON.parse(content) as Mission);
          this.missions.set(mission.id, mission);

          if (!this.sessionIndex.has(mission.sessionId)) {
            this.sessionIndex.set(mission.sessionId, new Set());
          }
          this.sessionIndex.get(mission.sessionId)!.add(mission.id);
        } catch (loadError) {
          logger.warn('MissionStorage.加载文件失败', {
            filePath,
            error: loadError instanceof Error ? loadError.message : String(loadError),
          }, LogCategory.ORCHESTRATOR);
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
    const normalizedMission = this.normalizeMission(mission);
    this.missions.set(mission.id, normalizedMission);

    if (!this.sessionIndex.has(mission.sessionId)) {
      this.sessionIndex.set(mission.sessionId, new Set());
    }
    this.sessionIndex.get(mission.sessionId)!.add(mission.id);

    await this.saveToDisk(normalizedMission);
  }

  async load(id: string): Promise<Mission | null> {
    await this.ensureLoaded();
    const mission = this.missions.get(id);
    return mission ? this.normalizeMission(mission) : null;
  }

  async update(mission: Mission): Promise<void> {
    await this.ensureLoaded();
    if (!this.missions.has(mission.id)) {
      throw new Error(`Mission not found: ${mission.id}`);
    }
    mission.updatedAt = Date.now();
    const normalizedMission = this.normalizeMission(mission);
    this.missions.set(mission.id, normalizedMission);
    await this.saveToDisk(normalizedMission);
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
