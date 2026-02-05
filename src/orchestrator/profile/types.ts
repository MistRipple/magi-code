/**
 * Worker Profile System - types
 */

import { WorkerSlot } from '../../types/agent-types';

// ============================================================================
// Worker persona types
// ============================================================================

/** Worker collaboration rules */
export interface WorkerCollaboration {
  /** 作为主导者时的行为 */
  asLeader: string[];
  /** 作为协作者时的行为 */
  asCollaborator: string[];
}

/** Built-in worker persona */
export interface WorkerPersona {
  /** 显示名称 */
  displayName: string;
  /** 角色定位 */
  baseRole: string;
  /** 擅长领域 */
  strengths: string[];
  /** 弱项领域 */
  weaknesses: string[];
  /** 协作规则 */
  collaboration: WorkerCollaboration;
  /** 输出偏好 */
  outputPreferences: string[];
  /** 推理过程要求 */
  reasoningGuidelines: string[];
}

/** Runtime worker profile (derived, non-configurable) */
export interface WorkerProfile {
  /** Worker 标识 */
  worker: WorkerSlot;
  /** 内置 persona */
  persona: WorkerPersona;
  /** 归属分类 */
  assignedCategories: string[];
}

// ============================================================================
// Category types
// ============================================================================

/** 任务优先级 */
export type TaskPriority = 'high' | 'medium' | 'low';

/** 风险等级 */
export type RiskLevel = 'high' | 'medium' | 'low';

/** 分类引导 */
export interface CategoryGuidance {
  focus: string[];
  constraints: string[];
}

/** 任务分类定义 */
export interface CategoryDefinition {
  displayName: string;
  description: string;
  keywords: string[];
  guidance: CategoryGuidance;
  priority: TaskPriority;
  riskLevel: RiskLevel;
}

/** 分类规则配置 */
export interface CategoryRules {
  categoryPriority: string[];
  defaultCategory: string;
  riskMapping: Record<RiskLevel, 'fullPath' | 'standardPath' | 'lightPath'>;
}

// ============================================================================
// Assignments
// ============================================================================

export interface WorkerAssignments {
  version: string;
  assignments: Record<WorkerSlot, string[]>;
}

// ============================================================================
// Guidance injection
// ============================================================================

/** 引导注入上下文 */
export interface InjectionContext {
  /** 任务描述 */
  taskDescription: string;
  /** 目标文件 */
  targetFiles?: string[];
  /** 依赖任务 */
  dependencies?: string[];
  /** 功能契约 */
  featureContract?: string;
  /** 协作者列表 */
  collaborators?: WorkerSlot[];
  /** 任务分类 */
  category?: string;
  /** 是否主导者 */
  isLeader?: boolean;
}
