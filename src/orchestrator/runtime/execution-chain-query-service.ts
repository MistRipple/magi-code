/**
 * 执行链查询服务
 *
 * 为上层消费者（UI、恢复、诊断）提供执行链的只读查询视图。
 * 不直接修改状态，只从 ExecutionChainStore 读取。
 */

import type { ExecutionChainRecord, ExecutionChainStatus, BranchRecord, AssignmentGroupRecord } from './execution-chain-types';
import type { ExecutionChainStore } from './execution-chain-store';

/**
 * 执行链概览（供 UI / 诊断使用）
 */
export interface ExecutionChainOverview {
  chainId: string;
  sessionId: string;
  status: ExecutionChainStatus;
  attempt: number;
  recoverable: boolean;
  currentMissionId?: string;
  currentPlanId?: string;
  activeAssignmentGroupId?: string;
  branchCount: number;
  assignmentGroupCount: number;
  createdAt: number;
  updatedAt: number;
}

export class ExecutionChainQueryService {
  constructor(private readonly store: ExecutionChainStore) {}

  /**
   * 获取指定 session 的执行链概览列表
   */
  getSessionOverview(sessionId: string): ExecutionChainOverview[] {
    const chains = this.store.getChainsBySession(sessionId);
    return chains.map((chain) => this.toOverview(chain));
  }

  /**
   * 获取当前正在运行的执行链（最多一条）
   */
  getRunningChain(sessionId: string): ExecutionChainRecord | undefined {
    const chains = this.store.getChainsBySession(sessionId);
    return chains.find((chain) => chain.status === 'running' || chain.status === 'resuming');
  }

  /**
   * 查找最近一条可恢复的被中断执行链
   *
   * 规则：
   *   - status === 'interrupted' && recoverable === true（正常中断路径）
   *   - status === 'paused'（governance 暂停：上游错误/预算超限等，防御性兼容旧数据）
   *   - 按 updatedAt 降序取最近一条
   *   - 不跨 session 查找
   */
  findLatestRecoverableChain(sessionId: string): ExecutionChainRecord | undefined {
    const chains = this.store.getChainsBySession(sessionId);
    return chains
      .filter((chain) => (chain.status === 'interrupted' && chain.recoverable) || chain.status === 'paused')
      .sort((a, b) => b.updatedAt - a.updatedAt)[0];
  }

  /**
   * 检查指定执行链是否可恢复
   */
  isChainRecoverable(chainId: string): boolean {
    const chain = this.store.getChain(chainId);
    if (!chain) return false;
    return (chain.status === 'interrupted' && chain.recoverable === true)
      || chain.status === 'paused';
  }

  /**
   * 获取执行链的完整分支树
   */
  getChainBranches(chainId: string): BranchRecord[] {
    return this.store.getBranchesByChain(chainId);
  }

  /**
   * 获取执行链的 AssignmentGroup 列表
   */
  getChainAssignmentGroups(chainId: string): AssignmentGroupRecord[] {
    return this.store.getAssignmentGroupsByChain(chainId);
  }

  /**
   * 获取执行链的主线分支
   */
  getMainlineBranch(chainId: string): BranchRecord | undefined {
    const branches = this.store.getBranchesByChain(chainId);
    return branches.find((branch) => branch.kind === 'mainline');
  }

  private toOverview(chain: ExecutionChainRecord): ExecutionChainOverview {
    return {
      chainId: chain.id,
      sessionId: chain.sessionId,
      status: chain.status,
      attempt: chain.attempt,
      recoverable: chain.recoverable,
      currentMissionId: chain.currentMissionId,
      currentPlanId: chain.currentPlanId,
      activeAssignmentGroupId: chain.activeAssignmentGroupId,
      branchCount: this.store.getBranchesByChain(chain.id).length,
      assignmentGroupCount: this.store.getAssignmentGroupsByChain(chain.id).length,
      createdAt: chain.createdAt,
      updatedAt: chain.updatedAt,
    };
  }
}

