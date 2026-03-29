/**
 * 执行链存储
 *
 * 内存态存储，按 sessionId 索引。
 * 阶段四将升级为持久化到 session.json。
 *
 * 职责：
 *   - 管理 ExecutionChainRecord、BranchRecord、AssignmentGroupRecord 的 CRUD
 *   - 保证同一 session 内 chainId 唯一
 *   - 提供状态流转保护（禁止非法转换）
 */

import type {
  ExecutionChainRecord,
  ExecutionChainStatus,
  BranchRecord,
  BranchKind,
  AssignmentGroupRecord,
  InterruptedReason,
} from './execution-chain-types';

// ============================================================================
// 合法状态流转表
// ============================================================================

const VALID_TRANSITIONS: Record<ExecutionChainStatus, ExecutionChainStatus[]> = {
  running:     ['paused', 'interrupted', 'completed', 'failed', 'cancelled'],
  paused:      ['running', 'interrupted', 'cancelled'],
  interrupted: ['resuming', 'cancelled'],
  resuming:    ['running', 'failed', 'interrupted'],
  completed:   [],
  failed:      [],
  cancelled:   [],
};

function isValidTransition(from: ExecutionChainStatus, to: ExecutionChainStatus): boolean {
  return VALID_TRANSITIONS[from]?.includes(to) ?? false;
}

function generateId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
}

// ============================================================================
// ExecutionChainStore
// ============================================================================

export class ExecutionChainStore {
  /** sessionId → chainId → record */
  private chains = new Map<string, Map<string, ExecutionChainRecord>>();
  /** chainId → branchId → record */
  private branches = new Map<string, Map<string, BranchRecord>>();
  /** chainId → groupId → record */
  private assignmentGroups = new Map<string, Map<string, AssignmentGroupRecord>>();

  // ===== Chain CRUD =====

  createChain(input: {
    sessionId: string;
    userMessageId: string;
    requestId: string;
  }): ExecutionChainRecord {
    const now = Date.now();
    const record: ExecutionChainRecord = {
      id: generateId('chain'),
      sessionId: input.sessionId,
      userMessageId: input.userMessageId,
      requestId: input.requestId,
      status: 'running',
      attempt: 0,
      recoverable: true,
      createdAt: now,
      updatedAt: now,
    };

    const sessionMap = this.chains.get(input.sessionId) ?? new Map<string, ExecutionChainRecord>();
    sessionMap.set(record.id, record);
    this.chains.set(input.sessionId, sessionMap);

    return record;
  }

  /**
   * 废弃同 session 下所有旧的 interrupted+recoverable 链。
   * 由 MDE 在非 resume 路径的新请求入口调用，确保"继续"语义仅对最近一轮有效。
   */
  expireRecoverableChains(sessionId: string): number {
    const sessionMap = this.chains.get(sessionId);
    if (!sessionMap) return 0;
    const now = Date.now();
    let expired = 0;
    for (const chain of sessionMap.values()) {
      if (chain.status === 'interrupted' && chain.recoverable) {
        chain.status = 'cancelled';
        chain.recoverable = false;
        chain.updatedAt = now;
        expired++;
      }
    }
    return expired;
  }

  getChain(chainId: string): ExecutionChainRecord | undefined {
    for (const sessionMap of this.chains.values()) {
      const record = sessionMap.get(chainId);
      if (record) return record;
    }
    return undefined;
  }

  getChainsBySession(sessionId: string): ExecutionChainRecord[] {
    const sessionMap = this.chains.get(sessionId);
    return sessionMap ? Array.from(sessionMap.values()) : [];
  }

  /**
   * 状态流转（带合法性校验）
   */
  transitionChainStatus(chainId: string, to: ExecutionChainStatus, options?: {
    interruptedReason?: InterruptedReason;
    recoverable?: boolean;
    missionId?: string;
    planId?: string;
    snapshotId?: string;
  }): ExecutionChainRecord {
    const record = this.getChain(chainId);
    if (!record) {
      throw new Error(`执行链不存在: ${chainId}`);
    }
    if (!isValidTransition(record.status, to)) {
      throw new Error(`执行链状态流转非法: ${record.status} → ${to} (chainId=${chainId})`);
    }

    record.status = to;
    record.updatedAt = Date.now();

    if (options?.interruptedReason !== undefined) {
      record.interruptedReason = options.interruptedReason;
    }
    if (options?.recoverable !== undefined) {
      record.recoverable = options.recoverable;
    }
    if (options?.missionId !== undefined) {
      record.currentMissionId = options.missionId;
    }
    if (options?.planId !== undefined) {
      record.currentPlanId = options.planId;
    }
    if (options?.snapshotId !== undefined) {
      record.latestSnapshotId = options.snapshotId;
    }

    // 终态时标记不可恢复
    if (to === 'completed' || to === 'cancelled' || to === 'failed') {
      record.recoverable = false;
    }
    // interrupted 默认可恢复（除非显式覆盖）
    if (to === 'interrupted' && options?.recoverable === undefined) {
      record.recoverable = true;
    }
    // resuming 时递增 attempt
    if (to === 'resuming') {
      record.attempt += 1;
    }

    return record;
  }

  /**
   * 更新执行链的绑定字段（mission/plan/assignmentGroup 等）
   */
  updateChainBindings(chainId: string, bindings: {
    currentMissionId?: string;
    currentPlanId?: string;
    activeAssignmentGroupId?: string;
  }): void {
    const record = this.getChain(chainId);
    if (!record) return;
    if (bindings.currentMissionId !== undefined) {
      record.currentMissionId = bindings.currentMissionId;
    }
    if (bindings.currentPlanId !== undefined) {
      record.currentPlanId = bindings.currentPlanId;
    }
    if (bindings.activeAssignmentGroupId !== undefined) {
      record.activeAssignmentGroupId = bindings.activeAssignmentGroupId;
    }
    record.updatedAt = Date.now();
  }

  // ===== Branch CRUD =====

  createBranch(input: {
    chainId: string;
    kind: BranchKind;
    parentBranchId?: string;
    workerSlot?: string;
    assignmentGroupId?: string;
  }): BranchRecord {
    const now = Date.now();
    const record: BranchRecord = {
      id: generateId('branch'),
      chainId: input.chainId,
      kind: input.kind,
      parentBranchId: input.parentBranchId,
      workerSlot: input.workerSlot,
      assignmentGroupId: input.assignmentGroupId,
      status: 'running',
      createdAt: now,
      updatedAt: now,
    };

    const chainMap = this.branches.get(input.chainId) ?? new Map<string, BranchRecord>();
    chainMap.set(record.id, record);
    this.branches.set(input.chainId, chainMap);

    return record;
  }

  getBranch(branchId: string): BranchRecord | undefined {
    for (const chainMap of this.branches.values()) {
      const record = chainMap.get(branchId);
      if (record) return record;
    }
    return undefined;
  }

  getBranchesByChain(chainId: string): BranchRecord[] {
    const chainMap = this.branches.get(chainId);
    return chainMap ? Array.from(chainMap.values()) : [];
  }

  transitionBranchStatus(branchId: string, to: ExecutionChainStatus): BranchRecord {
    const record = this.getBranch(branchId);
    if (!record) {
      throw new Error(`分支不存在: ${branchId}`);
    }
    if (!isValidTransition(record.status, to)) {
      throw new Error(`分支状态流转非法: ${record.status} → ${to} (branchId=${branchId})`);
    }
    record.status = to;
    record.updatedAt = Date.now();
    return record;
  }

  // ===== AssignmentGroup CRUD =====

  createAssignmentGroup(input: {
    chainId: string;
    dispatchBatchId?: string;
  }): AssignmentGroupRecord {
    const now = Date.now();
    const record: AssignmentGroupRecord = {
      id: generateId('agroup'),
      chainId: input.chainId,
      dispatchBatchId: input.dispatchBatchId,
      branchIds: [],
      status: 'active',
      createdAt: now,
      updatedAt: now,
    };

    const chainMap = this.assignmentGroups.get(input.chainId) ?? new Map<string, AssignmentGroupRecord>();
    chainMap.set(record.id, record);
    this.assignmentGroups.set(input.chainId, chainMap);

    // 同步更新 chain 的 activeAssignmentGroupId
    this.updateChainBindings(input.chainId, { activeAssignmentGroupId: record.id });

    return record;
  }

  getAssignmentGroup(groupId: string): AssignmentGroupRecord | undefined {
    for (const chainMap of this.assignmentGroups.values()) {
      const record = chainMap.get(groupId);
      if (record) return record;
    }
    return undefined;
  }

  getAssignmentGroupsByChain(chainId: string): AssignmentGroupRecord[] {
    const chainMap = this.assignmentGroups.get(chainId);
    return chainMap ? Array.from(chainMap.values()) : [];
  }

  addBranchToAssignmentGroup(groupId: string, branchId: string): void {
    const record = this.getAssignmentGroup(groupId);
    if (!record) return;
    if (!record.branchIds.includes(branchId)) {
      record.branchIds.push(branchId);
      record.updatedAt = Date.now();
    }
  }

  transitionAssignmentGroupStatus(groupId: string, to: AssignmentGroupRecord['status']): void {
    const record = this.getAssignmentGroup(groupId);
    if (!record) return;
    record.status = to;
    record.updatedAt = Date.now();
  }

  // ===== 启动恢复：收敛运行态 =====

  /**
   * 进程重启时，将所有 running/resuming 的链收敛。
   * 有快照 → interrupted + recoverable
   * 无快照 → failed + 不可恢复
   */
  convergeOnStartup(sessionId: string, hasSnapshot?: (chainId: string) => boolean): { converged: string[] } {
    const converged: string[] = [];
    const sessionMap = this.chains.get(sessionId);
    if (!sessionMap) return { converged };

    for (const record of sessionMap.values()) {
      if (record.status === 'running' || record.status === 'resuming') {
        const canRecover = hasSnapshot
          ? hasSnapshot(record.id)
          : Boolean(record.latestSnapshotId);
        if (canRecover) {
          record.status = 'interrupted';
          record.interruptedReason = 'process_exit';
          record.recoverable = true;
        } else {
          record.status = 'failed';
          record.recoverable = false;
        }
        record.updatedAt = Date.now();
        converged.push(record.id);
      }
    }

    return { converged };
  }

  // ===== 清理 =====

  clearSession(sessionId: string): void {
    const sessionMap = this.chains.get(sessionId);
    if (sessionMap) {
      for (const chainId of sessionMap.keys()) {
        this.branches.delete(chainId);
        this.assignmentGroups.delete(chainId);
      }
    }
    this.chains.delete(sessionId);
  }

  dispose(): void {
    this.chains.clear();
    this.branches.clear();
    this.assignmentGroups.clear();
  }

  // ===== 持久化序列化/反序列化 =====

  /**
   * 导出指定 session 的所有执行链数据（用于 session 持久化）
   */
  exportSession(sessionId: string): ExecutionChainSessionSnapshot | null {
    const sessionMap = this.chains.get(sessionId);
    if (!sessionMap || sessionMap.size === 0) return null;

    const chains: ExecutionChainRecord[] = [];
    const branches: BranchRecord[] = [];
    const groups: AssignmentGroupRecord[] = [];

    for (const chain of sessionMap.values()) {
      chains.push({ ...chain });
      const chainBranches = this.branches.get(chain.id);
      if (chainBranches) {
        for (const branch of chainBranches.values()) {
          branches.push({ ...branch });
        }
      }
      const chainGroups = this.assignmentGroups.get(chain.id);
      if (chainGroups) {
        for (const group of chainGroups.values()) {
          groups.push({ ...group, branchIds: [...group.branchIds] });
        }
      }
    }

    return { chains, branches, assignmentGroups: groups };
  }

  /**
   * 导入持久化数据到指定 session（用于 session 恢复）
   *
   * 注意：导入前会清除该 session 的现有数据。
   */
  importSession(sessionId: string, snapshot: ExecutionChainSessionSnapshot): void {
    this.clearSession(sessionId);

    const sessionMap = new Map<string, ExecutionChainRecord>();
    for (const chain of snapshot.chains) {
      sessionMap.set(chain.id, { ...chain });
    }
    this.chains.set(sessionId, sessionMap);

    for (const branch of snapshot.branches) {
      const chainMap = this.branches.get(branch.chainId) ?? new Map<string, BranchRecord>();
      chainMap.set(branch.id, { ...branch });
      this.branches.set(branch.chainId, chainMap);
    }

    for (const group of snapshot.assignmentGroups) {
      const chainMap = this.assignmentGroups.get(group.chainId) ?? new Map<string, AssignmentGroupRecord>();
      chainMap.set(group.id, { ...group, branchIds: [...group.branchIds] });
      this.assignmentGroups.set(group.chainId, chainMap);
    }
  }
}

/**
 * 持久化快照类型（写入 session.json）
 */
export interface ExecutionChainSessionSnapshot {
  chains: ExecutionChainRecord[];
  branches: BranchRecord[];
  assignmentGroups: AssignmentGroupRecord[];
}


