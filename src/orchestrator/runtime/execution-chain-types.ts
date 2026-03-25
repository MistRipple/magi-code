/**
 * 执行链核心类型定义
 *
 * 执行链（Execution Chain）是恢复能力的第一真相源。
 * 一次用户输入触发的一整轮主线执行构成一条执行链，
 * 该执行链可以跨多次"停止/继续"持续存在。
 *
 * 设计依据：execution-resume-architecture.md §6
 */

// ============================================================================
// 执行链状态
// ============================================================================

/**
 * 执行链状态
 *
 * 状态流转规则：
 *   running  → paused → running
 *   running  → interrupted → resuming → running
 *   running  → completed
 *   running  → failed
 *   running  → cancelled
 *   paused   → interrupted
 *   paused   → cancelled
 *   interrupted → resuming
 *   interrupted → cancelled
 *   resuming → running
 *   resuming → failed
 *   resuming → interrupted
 */
export type ExecutionChainStatus =
  | 'running'
  | 'paused'
  | 'interrupted'
  | 'resuming'
  | 'completed'
  | 'failed'
  | 'cancelled';

/**
 * 中断原因（底层原因，与业务终态解耦）
 *
 * 只在 status === 'interrupted' 或 status === 'failed' 时有意义。
 * 业务终态由 ExecutionChainStatus 承载，中断原因只描述"为什么中断"。
 */
export type InterruptedReason =
  | 'user_stop'          // 用户主动停止
  | 'process_exit'       // 进程退出/崩溃
  | 'extension_reload'   // 插件重载
  | 'external_abort';    // 外部中断（超时、治理等）

// ============================================================================
// 分支类型
// ============================================================================

/** 分支类别：主线或 Worker */
export type BranchKind = 'mainline' | 'worker';

// ============================================================================
// 核心记录类型
// ============================================================================

/**
 * 执行链记录
 *
 * 对应用户一次执行请求。执行链是停止/继续/放弃的操作目标。
 */
export interface ExecutionChainRecord {
  /** 执行链唯一 ID */
  id: string;
  /** 所属会话 */
  sessionId: string;
  /** 触发执行的用户消息 ID */
  userMessageId: string;
  /** 请求 ID（与编排器的 requestId 对应） */
  requestId: string;
  /** 执行链当前状态 */
  status: ExecutionChainStatus;
  /** 恢复尝试次数（首次执行为 0，每次 resume +1） */
  attempt: number;

  // ===== 关联实体 =====
  /** 当前关联的 Mission ID */
  currentMissionId?: string;
  /** 当前关联的 Plan ID */
  currentPlanId?: string;
  /** 当前活跃的 AssignmentGroup ID */
  activeAssignmentGroupId?: string;
  /** 最近一次恢复快照 ID */
  latestSnapshotId?: string;

  // ===== 中断与恢复 =====
  /** 中断原因（仅 interrupted/failed 时设置） */
  interruptedReason?: InterruptedReason;
  /** 是否可恢复 */
  recoverable: boolean;

  // ===== 时间戳 =====
  createdAt: number;
  updatedAt: number;
}

/**
 * 分支记录
 *
 * 执行链内部的执行分支。主线为一条 mainline 分支，
 * 每个 Worker 分配产生一条 worker 分支。
 */
export interface BranchRecord {
  /** 分支唯一 ID */
  id: string;
  /** 所属执行链 */
  chainId: string;
  /** 分支类别 */
  kind: BranchKind;
  /** 父分支 ID（Worker 分支的父分支为 mainline） */
  parentBranchId?: string;
  /** Worker 标识（仅 worker 分支） */
  workerSlot?: string;
  /** 关联的 AssignmentGroup ID（仅 worker 分支） */
  assignmentGroupId?: string;
  /** 分支状态（与执行链状态同步，但可独立终止） */
  status: ExecutionChainStatus;

  createdAt: number;
  updatedAt: number;
}

/**
 * Assignment Group 记录
 *
 * Worker 卡片分组的核心单位。一次主线分配 Worker 的动作形成一个 AssignmentGroup。
 * 同一 AssignmentGroup 内，同一 workerSlot 的串行任务合并到一个 Worker 卡片。
 * 新一轮 Worker 分配必须生成新的 AssignmentGroup。
 *
 * Worker 卡片 key = assignmentGroupId + workerSlot
 */
export interface AssignmentGroupRecord {
  /** 唯一 ID */
  id: string;
  /** 所属执行链 */
  chainId: string;
  /** 关联的 dispatch batch ID（与现有 dispatch 系统桥接） */
  dispatchBatchId?: string;
  /** 该组内的 Worker 分支 ID 列表 */
  branchIds: string[];
  /** 该组的状态 */
  status: 'active' | 'completed' | 'interrupted' | 'cancelled';

  createdAt: number;
  updatedAt: number;
}

