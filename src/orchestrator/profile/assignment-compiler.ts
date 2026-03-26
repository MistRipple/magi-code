/**
 * AssignmentCompiler — 任务编译器
 *
 * 定位：LLM hint + 确定性编译器。
 * 在 worker_dispatch 注册链路中，将编排器的自由文本 hint
 * 编译为确定性的 ownership + mode + worker 三元组。
 *
 * 设计原则：
 * - 以 Orchestrator 的 ownership_hint / mode_hint 为首要输入
 * - 用规则做校验、归一化、跨域自动拆分检测
 * - hint=auto 时退回规则推导
 * - 全部确定性逻辑，不额外调 LLM
 * - 编译结果一经产出即为最终身份——"首次渲染即最终身份"
 */

import type { WorkerSlot } from '../../types/agent-types';
import type { TaskOwnership, TaskMode, TaskClassification } from './task-taxonomy';

// ============================================================================
// 编译器输入
// ============================================================================

/** 编排器通过 worker_dispatch 传入的 hint */
export interface AssignmentCompilerInput {
  /** ownership hint（LLM 提供的归属建议，可为 auto） */
  ownershipHint: string;
  /** mode hint（LLM 提供的执行模式建议，可为 auto） */
  modeHint: string;
  /** 任务标题 */
  taskTitle: string;
  /** 任务目标 */
  goal: string;
  /** 已知上下文 */
  context: string[];
  /** 约束条件 */
  constraints: string[];
}

// ============================================================================
// 编译器输出
// ============================================================================

/** 单个编译结果 */
export interface AssignmentCompilationItem {
  /** 编译后的分类 */
  classification: TaskClassification;
  /** 选中的 Worker */
  selectedWorker: WorkerSlot;
  /** 路由决策原因（可追溯） */
  routingReason: string;
  /** 子任务标题建议（跨域拆分时生成） */
  suggestedTaskTitle?: string;
  /** 子任务目标建议（跨域拆分时生成） */
  suggestedGoal?: string;
}

/** 编译成功结果 */
export interface AssignmentCompilationSuccess {
  ok: true;
  /** 编译结果列表（跨域拆分时可能有多个） */
  items: AssignmentCompilationItem[];
  /** hint 是否被覆盖 */
  hintOverridden: boolean;
  /** 覆盖说明（仅 hintOverridden=true 时有值） */
  overrideDetail?: string;
  /** 是否发生了自动拆分 */
  autoSplit: boolean;
}

/** 编译拒绝结果（无法编译的场景） */
export interface AssignmentCompilationRejection {
  ok: false;
  /** 拒绝原因（返回给编排器的错误文本） */
  error: string;
  /** 拒绝码 */
  rejectionCode: 'no_available_worker' | 'invalid_input';
}

export type AssignmentCompilationResult = AssignmentCompilationSuccess | AssignmentCompilationRejection;

// ============================================================================
// 编译器接口
// ============================================================================

export interface IAssignmentCompiler {
  /**
   * 将 hint + 任务文本编译为确定性的 ownership + mode + worker。
   *
   * 编译顺序：
   * 1. 解析并归一化 hint（auto / 具体值 / 无效值回退）
   * 2. 推断 ownership（hint 优先 → 文本推断 → 默认 general）
   * 3. 跨域检测（frontend + backend 同时出现 → 自动拆分为多个 item）
   * 4. 推断 mode（hint 优先 → 文本推断 → 默认 implement）
   * 5. 基于 ownership 选择 worker
   */
  compile(input: AssignmentCompilerInput): AssignmentCompilationResult;
}

