/**
 * Worker Profile System - 类型定义
 *
 * 设计理念：
 * - 引导而非限制：通过 Prompt 注入引导 Worker 行为，不限制工具权限
 * - CLI 进程复用：利用成熟 CLI 的完整能力
 * - 执行统计驱动：基于历史数据动态调整 Worker 选择
 */

import { CLIType } from '../../types';

// ============================================================================
// Worker 画像类型
// ============================================================================

/** Worker 能力画像 */
export interface WorkerCapabilityProfile {
  /** 擅长的领域 */
  strengths: string[];
  /** 相对不擅长的领域 */
  weaknesses: string[];
}

/** Worker 任务偏好 */
export interface WorkerPreferences {
  /** 优先分配的任务分类 */
  preferredCategories: string[];
  /** 优先分配的关键词（正则模式） */
  preferredKeywords: string[];
}

/** Worker 行为引导 */
export interface WorkerGuidance {
  /** 角色定位 */
  role: string;
  /** 专注领域 */
  focus: string[];
  /** 行为约束（建议性） */
  constraints: string[];
  /** 输出格式偏好 */
  outputPreferences: string[];
}

/** Worker 协作规则 */
export interface WorkerCollaboration {
  /** 作为主导者时的行为 */
  asLeader: string[];
  /** 作为协作者时的行为 */
  asCollaborator: string[];
}

/** Worker 评审配置 */
export interface WorkerReviewConfig {
  /** 被评审时需要重点关注的领域 */
  focusAreasWhenReviewed: string[];
  /** 作为评审者时的优势领域 */
  reviewStrengths: string[];
  /** 需要严格评审的任务分类 */
  strictReviewCategories: string[];
}

/** 完整的 Worker 画像 */
export interface WorkerProfile {
  /** Worker 名称 */
  name: string;
  /** 显示名称 */
  displayName: string;
  /** 配置版本 */
  version: string;
  /** 能力画像 */
  profile: WorkerCapabilityProfile;
  /** 任务偏好 */
  preferences: WorkerPreferences;
  /** 行为引导 */
  guidance: WorkerGuidance;
  /** 协作规则 */
  collaboration: WorkerCollaboration;
  /** 评审配置（可选，向后兼容） */
  review?: WorkerReviewConfig;
}

// ============================================================================
// 任务分类类型
// ============================================================================

/** 任务优先级 */
export type TaskPriority = 'high' | 'medium' | 'low';

/** 风险等级 */
export type RiskLevel = 'high' | 'medium' | 'low';

/** 评审策略配置 */
export interface CategoryReviewPolicy {
  /** 是否需要互检评审 */
  requirePeerReview: boolean;
  /** 偏好的评审者 */
  preferredReviewer?: CLIType;
  /** 评审重点 */
  reviewFocus: string[];
}

/** 任务分类配置 */
export interface CategoryConfig {
  /** 显示名称 */
  displayName: string;
  /** 描述 */
  description: string;
  /** 匹配关键词（正则模式） */
  keywords: string[];
  /** 默认 Worker */
  defaultWorker: CLIType;
  /** 优先级 */
  priority: TaskPriority;
  /** 风险等级 */
  riskLevel: RiskLevel;
  /** 评审策略（可选，向后兼容） */
  reviewPolicy?: CategoryReviewPolicy;
}

/** 分类规则配置 */
export interface CategoryRules {
  /** 分类优先级顺序 */
  categoryPriority: string[];
  /** 默认分类 */
  defaultCategory: string;
  /** 风险等级映射 */
  riskMapping: Record<RiskLevel, 'fullPath' | 'standardPath' | 'lightPath'>;
}

/** 完整的分类配置 */
export interface CategoriesConfig {
  /** 配置版本 */
  version: string;
  /** 分类定义 */
  categories: Record<string, CategoryConfig>;
  /** 分类规则 */
  rules: CategoryRules;
}

// ============================================================================
// 引导注入类型
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
  collaborators?: CLIType[];
  /** 任务分类 */
  category?: string;
}

// ============================================================================
// Worker 选择类型
// ============================================================================

/** Worker 选择选项 */
export interface WorkerSelectionOptions {
  /** 排除的 Worker */
  excludeWorkers?: CLIType[];
  /** 偏好的 Worker */
  preferredWorker?: CLIType;
}

/** Worker 选择结果 */
export interface WorkerSelectionResult {
  /** 选中的 Worker */
  worker: CLIType;
  /** 任务分类 */
  category: string;
  /** 匹配分数 */
  score: number;
  /** 选择原因 */
  reason: string;
}

