/**
 * AssignmentCompiler — 任务编译器（实现）
 *
 * 确定性编译流水线：
 * 1. 归一化 hint（auto / 具体值 / 无效值回退）
 * 2. 推断 ownership（hint 优先 → 文本推断 → 默认 general）
 * 3. 跨域检测（frontend + backend 同时出现 → 自动拆分）
 * 4. 推断 mode（hint 优先 → 文本推断 → 默认 implement）
 * 5. 基于 ownership 选择 worker
 */

import type { WorkerSlot } from '../../types/agent-types';
import {
  type TaskOwnership,
  type TaskMode,
  TASK_OWNERSHIPS,
  TASK_MODES,
  DEFAULT_OWNERSHIP,
  DEFAULT_MODE,
  OWNERSHIP_SPLIT_BOUNDARY,
  isValidOwnership,
  isValidMode,
} from './task-taxonomy';
import type {
  IAssignmentCompiler,
  AssignmentCompilerInput,
  AssignmentCompilationResult,
} from './assignment-compiler';
import { DomainDetector, type OwnershipDomain } from './domain-detector';

// ============================================================================
// ownership 域 → TaskOwnership 映射
// ============================================================================

const DOMAIN_TO_OWNERSHIP: Record<OwnershipDomain, TaskOwnership> = {
  frontend: 'frontend',
  backend: 'backend',
  integration: 'integration',
  test: 'general',       // test 不是 ownership，是 mode
  document: 'general',   // document 不是 ownership，是 mode
  data_analysis: 'data_analysis',
};

/** 旧 category 值 → TaskOwnership 的映射（兼容编排器产出的 hint） */
const LEGACY_CATEGORY_TO_OWNERSHIP: Record<string, TaskOwnership> = {
  frontend: 'frontend',
  backend: 'backend',
  integration: 'integration',
  data_analysis: 'data_analysis',
  // 以下旧 category 都映射为 general（mode 从另一轴推断）
  implement: 'general',
  simple: 'general',
  general: 'general',
  bugfix: 'general',
  architecture: 'general',
  refactor: 'general',
  review: 'general',
  debug: 'general',
  test: 'general',
  document: 'general',
};

/** 旧 category 值 → TaskMode 的映射（兼容编排器产出的 hint） */
const LEGACY_CATEGORY_TO_MODE: Record<string, TaskMode> = {
  architecture: 'architecture',
  refactor: 'refactor',
  review: 'review',
  debug: 'debug',
  bugfix: 'debug',
  test: 'test',
  document: 'document',
  implement: 'implement',
  simple: 'implement',
  general: 'implement',
};

// ============================================================================
// mode 文本推断
// ============================================================================

interface ModeSignal {
  mode: TaskMode;
  patterns: RegExp[];
}

const MODE_SIGNALS: ModeSignal[] = [
  { mode: 'test', patterns: [/测试|test|spec|单元测试|e2e|mock|断言|覆盖率|jest|vitest|mocha/i] },
  { mode: 'review', patterns: [/审查|review|代码检查|code review/i] },
  { mode: 'debug', patterns: [/调试|debug|排查|诊断|修复.*bug|bugfix|故障/i] },
  { mode: 'refactor', patterns: [/重构|refactor|优化.*结构|解耦|抽象/i] },
  { mode: 'document', patterns: [/文档|document|readme|注释|说明|指南|教程/i] },
  { mode: 'architecture', patterns: [/架构|architecture|系统设计|模块设计|技术方案/i] },
  // implement 是默认值，不需要信号检测
];

function inferModeFromText(text: string): TaskMode {
  const normalized = text.toLowerCase();
  for (const { mode, patterns } of MODE_SIGNALS) {
    for (const pattern of patterns) {
      if (pattern.test(normalized)) {
        return mode;
      }
    }
  }
  return DEFAULT_MODE;
}

// ============================================================================
// AssignmentCompiler 实现
// ============================================================================

export class AssignmentCompilerImpl implements IAssignmentCompiler {
  private readonly domainDetector = new DomainDetector();
  /**
   * category → worker 路由表（直接来自用户配置的 worker-assignments.json）
   *
   * 这是唯一的路由真相来源。LLM 产出的 ownershipHint / modeHint
   * 本质上就是 category 名称，直接查表即可得到目标 worker。
   *
   * 例如：用户配置 codex → [bugfix, debug, review]
   *       → categoryMap: { debug: codex, review: codex, bugfix: codex }
   *       → LLM 发出 modeHint='debug' → 查表 → codex
   */
  private readonly categoryMap: ReadonlyMap<string, WorkerSlot>;

  constructor(
    private readonly fallbackWorker: WorkerSlot,
    private readonly availableWorkers: ReadonlySet<WorkerSlot>,
    categoryWorkerMap: ReadonlyMap<string, WorkerSlot>,
  ) {
    this.categoryMap = categoryWorkerMap;
  }

  compile(input: AssignmentCompilerInput): AssignmentCompilationResult {
    // ── Step 1: 归一化 hint ──
    const normalizedOwnershipHint = this.normalizeHint(input.ownershipHint);
    const normalizedModeHint = this.normalizeHint(input.modeHint);

    // ── Step 2: 推断 ownership ──
    let ownership: TaskOwnership;
    let hintOverridden = false;
    let overrideDetail: string | undefined;

    if (normalizedOwnershipHint && isValidOwnership(normalizedOwnershipHint)) {
      // hint 是有效的 ownership 值
      ownership = normalizedOwnershipHint;
    } else if (normalizedOwnershipHint && LEGACY_CATEGORY_TO_OWNERSHIP[normalizedOwnershipHint]) {
      // hint 是旧 category 值（如 'test', 'review'），映射为 ownership
      ownership = LEGACY_CATEGORY_TO_OWNERSHIP[normalizedOwnershipHint];
    } else {
      // hint 为 auto 或无效 → 从任务文本推断
      ownership = this.inferOwnershipFromText(input);
    }

    // ── Step 3: 跨域检测与自动拆分 ──
    const textParts = [input.taskTitle, input.goal, ...input.context];
    const detection = this.domainDetector.detectFromTextParts(textParts);

    // 如果 hint 明确指定了一个有效域，信任 hint，不做拆分
    if (OWNERSHIP_SPLIT_BOUNDARY.has(ownership) && detection.splitBoundaryDomains.includes(ownership as OwnershipDomain)) {
      // hint 指定的域在检测到的边界域中，信任 hint，继续单任务编译
    } else if (detection.splitBoundaryDomains.length > 1) {
      // 检测到多个边界域（如 frontend + backend），自动拆分
      return this.compileSplitAssignments(input, detection.splitBoundaryDomains, hintOverridden, overrideDetail);
    }

    // ── Step 4: 推断 mode ──
    let mode: TaskMode;
    if (normalizedModeHint && isValidMode(normalizedModeHint)) {
      mode = normalizedModeHint;
    } else if (normalizedModeHint && LEGACY_CATEGORY_TO_MODE[normalizedModeHint]) {
      mode = LEGACY_CATEGORY_TO_MODE[normalizedModeHint];
    } else if (normalizedOwnershipHint && LEGACY_CATEGORY_TO_MODE[normalizedOwnershipHint]) {
      // ownership hint 隐含了 mode 信息（如 hint='test' → mode=test）
      mode = LEGACY_CATEGORY_TO_MODE[normalizedOwnershipHint];
    } else {
      mode = inferModeFromText([input.taskTitle, input.goal].join(' '));
    }

    // ── Step 5: 选择 worker（mode 亲和性优先） ──
    const selectedWorker = this.selectWorker(ownership, mode);
    if (!selectedWorker) {
      return {
        ok: false,
        error: `没有可用的 Worker 来执行 ownership=${ownership} 的任务。`,
        rejectionCode: 'no_available_worker',
      };
    }

    return {
      ok: true,
      items: [
        {
          classification: { ownership, mode },
          selectedWorker,
          routingReason: `ownership=${ownership}, mode=${mode} → ${selectedWorker}`,
        },
      ],
      hintOverridden,
      overrideDetail,
      autoSplit: false,
    };
  }

  // ── 内部方法 ──

  private normalizeHint(hint: string): string | null {
    if (!hint || typeof hint !== 'string') return null;
    const normalized = hint.trim().toLowerCase().replace(/[\s-]+/g, '_');
    if (normalized === 'auto' || normalized === 'mixed' || normalized === '') return null;
    return normalized;
  }

  private inferOwnershipFromText(input: AssignmentCompilerInput): TaskOwnership {
    const textParts = [input.taskTitle, input.goal, ...input.context];
    const detection = this.domainDetector.detectFromTextParts(textParts);

    if (detection.matchedDomains.length === 0) {
      return DEFAULT_OWNERSHIP;
    }

    // 优先选择边界域（frontend/backend/integration）
    for (const domain of detection.matchedDomains) {
      if (OWNERSHIP_SPLIT_BOUNDARY.has(DOMAIN_TO_OWNERSHIP[domain])) {
        return DOMAIN_TO_OWNERSHIP[domain];
      }
    }

    // 否则取第一个匹配的域
    return DOMAIN_TO_OWNERSHIP[detection.matchedDomains[0]] || DEFAULT_OWNERSHIP;
  }

  /**
   * 从 categoryMap 直接查表选择 worker
   *
   * 优先级链：
   * 1. 专用 ownership 精确匹配（frontend/backend/integration/data_analysis）
   *    —— 这些域有明确的职责归属，ownership 是最强路由信号
   * 2. mode 精确匹配（debug/review/test/implement 等）
   *    —— 当 ownership 为 general（通用域）时，mode 是更精确的路由信号
   * 3. fallback → general category
   * 4. fallback worker
   * 5. 任意可用 worker
   *
   * 设计逻辑：
   * - ownership=frontend 的任务，即使 mode=implement → 也走 gemini（因为 frontend 是专用域）
   * - ownership=general + mode=debug 的任务 → 走 codex（因为 general 是通用域，mode 更精确）
   * - ownership=general + mode=implement 的任务 → 走 claude（implement → claude）
   *
   * categoryMap 就是用户配置的 worker-assignments 的直接映射，
   * 不做任何中间抽象——配什么就查什么。
   */
  private static readonly SPECIFIC_OWNERSHIPS = new Set<string>(['frontend', 'backend', 'integration', 'data_analysis']);

  private selectWorker(ownership: TaskOwnership, mode?: TaskMode): WorkerSlot | null {
    // 1. 专用 ownership 精确查表（非 general 的具体域）
    if (AssignmentCompilerImpl.SPECIFIC_OWNERSHIPS.has(ownership)) {
      const ownershipWorker = this.lookupAvailableWorker(ownership);
      if (ownershipWorker) return ownershipWorker;
    }

    // 2. mode 精确查表（ownership 为 general 或专用域未命中时）
    if (mode) {
      const modeWorker = this.lookupAvailableWorker(mode);
      if (modeWorker) return modeWorker;
    }

    // 3. ownership 查表（含 general 兜底）
    const ownershipWorker = this.lookupAvailableWorker(ownership);
    if (ownershipWorker) return ownershipWorker;

    // 4. fallback → general category
    if (ownership !== 'general') {
      const generalWorker = this.lookupAvailableWorker('general');
      if (generalWorker) return generalWorker;
    }

    // 5. fallback worker
    if (this.availableWorkers.has(this.fallbackWorker)) {
      return this.fallbackWorker;
    }

    // 6. 任意可用 worker
    for (const worker of this.availableWorkers) {
      return worker;
    }

    return null;
  }

  /**
   * 在 categoryMap 中查找 category 对应的 worker，仅返回当前可用的
   */
  private lookupAvailableWorker(category: string): WorkerSlot | null {
    const worker = this.categoryMap.get(category);
    if (worker && this.availableWorkers.has(worker)) {
      return worker;
    }
    return null;
  }

  /**
   * 编译拆分后的多个 Assignment
   * 当检测到跨域任务时，自动生成多个子任务
   */
  private compileSplitAssignments(
    input: AssignmentCompilerInput,
    domains: OwnershipDomain[],
    hintOverridden: boolean,
    overrideDetail?: string,
  ): AssignmentCompilationResult {
    const items: import('./assignment-compiler').AssignmentCompilationItem[] = [];

    for (const domain of domains) {
      const ownership = DOMAIN_TO_OWNERSHIP[domain];
      if (!isValidOwnership(ownership)) continue;

      // 推断 mode
      const mode = this.inferModeForSplitTask(input, ownership);

      const selectedWorker = this.selectWorker(ownership, mode);
      if (!selectedWorker) continue;

      // 生成子任务建议
      const suggestedTaskTitle = this.generateSplitTaskTitle(input.taskTitle, ownership);
      const suggestedGoal = this.generateSplitTaskGoal(input.goal, ownership);

      items.push({
        classification: { ownership, mode },
        selectedWorker,
        routingReason: `auto-split: ownership=${ownership}, mode=${mode} → ${selectedWorker}`,
        suggestedTaskTitle,
        suggestedGoal,
      });
    }

    if (items.length === 0) {
      return {
        ok: false,
        error: `检测到跨域任务但没有可用的 Worker 来执行。`,
        rejectionCode: 'no_available_worker',
      };
    }

    return {
      ok: true,
      items,
      hintOverridden,
      overrideDetail,
      autoSplit: true,
    };
  }

  private inferModeForSplitTask(input: AssignmentCompilerInput, ownership: TaskOwnership): TaskMode {
    // 先尝试从 modeHint 推断
    if (input.modeHint) {
      const normalized = this.normalizeHint(input.modeHint);
      if (normalized && isValidMode(normalized)) {
        return normalized;
      }
      if (normalized && LEGACY_CATEGORY_TO_MODE[normalized]) {
        return LEGACY_CATEGORY_TO_MODE[normalized];
      }
    }

    // 从文本推断
    const mode = inferModeFromText([input.taskTitle, input.goal].join(' '));
    return mode;
  }

  private generateSplitTaskTitle(originalTitle: string, ownership: TaskOwnership): string {
    // 简单策略：在原标题前加 ownership 前缀
    const prefix = ownership === 'frontend' ? '[Frontend] ' :
                   ownership === 'backend' ? '[Backend] ' :
                   ownership === 'integration' ? '[Integration] ' :
                   ownership === 'data_analysis' ? '[Data] ' : '';

    // 如果已经有前缀，不再重复添加
    if (originalTitle.toLowerCase().includes(`[${ownership}]`)) {
      return originalTitle;
    }

    return `${prefix}${originalTitle}`;
  }

  private generateSplitTaskGoal(originalGoal: string, ownership: TaskOwnership): string {
    // 简单策略：根据 ownership 生成目标提示
    const focusHint = ownership === 'frontend' ? '（前端部分：UI 组件、交互逻辑）' :
                      ownership === 'backend' ? '（后端部分：API、数据处理）' :
                      ownership === 'integration' ? '（集成部分：联调、端到端）' :
                      ownership === 'data_analysis' ? '（数据分析部分）' : '';

    return `${originalGoal} ${focusHint}`;
  }
}
