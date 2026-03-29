/**
 * 恢复快照构建器
 *
 * 在安全边界处收集运行时状态，构建 ResumeSnapshot。
 * 不直接读取散落状态，而是由调用方显式传入上下文。
 */

import type {
  ResumeSnapshot,
  ResumeMainlineContext,
  ResumeDispatchContext,
  ResumeWorkerBranchContext,
  ResumeWorkspaceContext,
  ResumeTimelineCursor,
} from './resume-snapshot-types';
import type { ExecutionChainRecord } from './execution-chain-types';
import type { ResumeSnapshotStore } from './resume-snapshot-store';

function generateSnapshotId(): string {
  return `snap_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
}

export interface ResumeSnapshotBuildInput {
  /** 当前执行链记录 */
  chain: ExecutionChainRecord;

  /** 主线恢复上下文 */
  mainline: ResumeMainlineContext;

  /** Dispatch 恢复上下文 */
  dispatch: ResumeDispatchContext;

  /** Worker 分支恢复上下文列表 */
  workerBranches: ResumeWorkerBranchContext[];

  /** 工作区状态 */
  workspace: ResumeWorkspaceContext;

  /** 时间轴游标 */
  timelineCursor: ResumeTimelineCursor;
}

export class ResumeSnapshotBuilder {
  constructor(private readonly store: ResumeSnapshotStore) {}

  /**
   * 构建并持久化一个恢复快照
   *
   * 调用时机：
   *   - 主线进入新阶段前后
   *   - dispatch batch 创建后
   *   - assignment group 状态变化后
   *   - Worker 任务完成后
   *   - 工具调用完成后
   *   - 用户点击停止并完成 quiesce 后
   */
  build(input: ResumeSnapshotBuildInput): ResumeSnapshot {
    const checkpointSeq = this.store.getNextCheckpointSeq(input.chain.id, input.chain.attempt);

    const snapshot: ResumeSnapshot = {
      id: generateSnapshotId(),
      chainId: input.chain.id,
      attempt: input.chain.attempt,
      checkpointSeq,
      mainline: {
        currentMissionId: input.mainline.currentMissionId,
        currentPlanId: input.mainline.currentPlanId,
        runtimePhase: input.mainline.runtimePhase,
        pendingSupplementaryInputs: [...input.mainline.pendingSupplementaryInputs],
        contextDigest: [...input.mainline.contextDigest],
      },
      dispatch: {
        assignmentGroupId: input.dispatch.assignmentGroupId,
        pendingTaskIds: [...input.dispatch.pendingTaskIds],
        runningTaskIds: [...input.dispatch.runningTaskIds],
        completedTaskIds: [...input.dispatch.completedTaskIds],
        cancelledTaskIds: input.dispatch.cancelledTaskIds ? [...input.dispatch.cancelledTaskIds] : [],
        completedWorkerSummaries: input.dispatch.completedWorkerSummaries
          ? input.dispatch.completedWorkerSummaries.map(s => ({ ...s }))
          : [],
      },
      workerBranches: input.workerBranches.map((wb) => ({
        branchId: wb.branchId,
        workerSlot: wb.workerSlot,
        assignmentGroupId: wb.assignmentGroupId,
        workerSessionId: wb.workerSessionId,
        currentTodoId: wb.currentTodoId,
        completedTodoIds: [...wb.completedTodoIds],
        pendingTodoIds: [...wb.pendingTodoIds],
        contextDigest: [...wb.contextDigest],
        latestSummary: wb.latestSummary,
      })),
      workspace: {
        dirtyFiles: [...input.workspace.dirtyFiles],
        pendingChangesSummary: [...input.workspace.pendingChangesSummary],
      },
      timelineCursor: {
        lastVisibleNodeSeq: input.timelineCursor.lastVisibleNodeSeq,
      },
      createdAt: Date.now(),
    };

    this.store.save(snapshot);
    return snapshot;
  }
}

