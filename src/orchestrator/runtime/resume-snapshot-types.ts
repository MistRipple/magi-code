/**
 * 恢复快照类型定义
 *
 * 恢复不应直接读取散落状态，而应读取标准快照。
 * ResumeSnapshot 记录了从中断点恢复所需的最小充分信息。
 *
 * 设计依据：execution-resume-architecture.md §6.4
 */

/**
 * 恢复快照
 *
 * 在安全边界持久化，供 resume 流程读取。
 * 快照写入时机：
 *   - 主线进入新阶段前后
 *   - dispatch batch 创建后
 *   - assignment group 状态变化后
 *   - Worker 任务完成后
 *   - 工具调用完成后
 *   - 用户点击停止并完成 quiesce 后
 */
export interface ResumeSnapshot {
  /** 快照唯一 ID */
  id: string;
  /** 所属执行链 */
  chainId: string;
  /** 产生此快照时的 attempt 序号 */
  attempt: number;
  /** 检查点序号（同一 attempt 内单调递增） */
  checkpointSeq: number;

  // ===== 主线恢复上下文 =====
  mainline: ResumeMainlineContext;

  // ===== Dispatch 恢复上下文 =====
  dispatch: ResumeDispatchContext;

  // ===== Worker 分支恢复上下文 =====
  workerBranches: ResumeWorkerBranchContext[];

  // ===== 工作区状态 =====
  workspace: ResumeWorkspaceContext;

  // ===== 时间轴游标 =====
  timelineCursor: ResumeTimelineCursor;

  createdAt: number;
}

/**
 * 主线恢复上下文
 *
 * 恢复时不重新立项、不重新生成 chainId、不重新全量读上下文。
 */
export interface ResumeMainlineContext {
  /** 中断时的原始用户 prompt */
  userPrompt?: string;
  /** 当前 Mission ID */
  currentMissionId?: string;
  /** 当前 Plan ID */
  currentPlanId?: string;
  /** 运行阶段标识（如 planning / dispatching / reviewing） */
  runtimePhase?: string;
  /** 未消费的补充指令 */
  pendingSupplementaryInputs: string[];
  /** 主线上下文摘要（已完成阶段、已生成总结等） */
  contextDigest: string[];
}

/**
 * Dispatch 恢复上下文
 *
 * 记录 Worker 分配的分组状态。
 */
export interface ResumeDispatchContext {
  /** 当前 AssignmentGroup ID */
  assignmentGroupId?: string;
  /** 等待中的任务 ID */
  pendingTaskIds: string[];
  /** 运行中的任务 ID */
  runningTaskIds: string[];
  /** 已完成的任务 ID */
  completedTaskIds: string[];
  /** 被取消的任务 ID */
  cancelledTaskIds?: string[];
  /** 已完成 Worker 的结果摘要（resume 时嵌入 prompt，避免 LLM 重做已完成工作） */
  completedWorkerSummaries?: Array<{
    taskId: string;
    worker: string;
    taskTitle: string;
    summary: string;
  }>;
}

/**
 * Worker 分支恢复上下文
 *
 * 每个 Worker 分支一条记录。恢复时沿用原 branchId 和 assignmentGroupId。
 */
export interface ResumeWorkerBranchContext {
  /** 分支 ID */
  branchId: string;
  /** Worker 标识 */
  workerSlot: string;
  /** 所属 AssignmentGroup */
  assignmentGroupId: string;
  /** Worker 会话 ID（用于沿用原 Worker session） */
  workerSessionId?: string;
  /** 当前正在执行的 todo ID */
  currentTodoId?: string;
  /** 已完成的 todo ID 列表 */
  completedTodoIds: string[];
  /** 未开始的 todo ID 列表 */
  pendingTodoIds: string[];
  /** Worker 上下文摘要（已读文件、阶段结论等） */
  contextDigest: string[];
  /** 最近一次 Worker 总结 */
  latestSummary?: string;
}

/**
 * 工作区恢复上下文
 */
export interface ResumeWorkspaceContext {
  /** 已修改但未提交的文件路径 */
  dirtyFiles: string[];
  /** 待处理变更摘要 */
  pendingChangesSummary: string[];
}

/**
 * 时间轴游标
 *
 * 标记最后一个用户可见的时间轴节点位置，用于 resume 后续接。
 */
export interface ResumeTimelineCursor {
  /** 最后一个可见节点的序号 */
  lastVisibleNodeSeq: number;
}

