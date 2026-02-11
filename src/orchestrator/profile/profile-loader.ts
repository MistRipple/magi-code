/**
 * Worker Profile System - 画像加载器（单一数据源版）
 *
 * 核心逻辑：
 * - 内置 personas 提供角色身份 + 工作方法 + 行为约束（纯 LLM 行为矫正）
 * - strengths/weaknesses 从 assignedCategories 动态推导，不再硬编码
 * - 推导规则：strengths = 分配分类的 displayName；weaknesses = 未分配的高/中优先级分类
 */

import { WorkerSlot } from '../../types/agent-types';
import { logger, LogCategory } from '../../logging';
import { WorkerProfile, CategoryDefinition, CategoryRules } from './types';
import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';
import { CATEGORY_RULES } from './builtin/category-rules';
import { WorkerAssignmentLoader } from './worker-assignments';
import { WORKER_PERSONAS } from './builtin/worker-personas';

export class ProfileLoader {
  private profiles: Map<WorkerSlot, WorkerProfile> = new Map();
  private categories: Map<string, CategoryDefinition> = new Map();
  private loaded = false;
  private assignmentLoader = new WorkerAssignmentLoader();

  /** 单例实例 */
  private static instance: ProfileLoader | null = null;

  static getInstance(): ProfileLoader {
    if (!ProfileLoader.instance) {
      ProfileLoader.instance = new ProfileLoader();
    }
    return ProfileLoader.instance;
  }

  static resetInstance(): void {
    ProfileLoader.instance = null;
  }

  private constructor() {
    this.categories = new Map(Object.entries(CATEGORY_DEFINITIONS));
  }

  async load(): Promise<void> {
    if (this.loaded) return;

    const assignments = this.assignmentLoader.load();
    this.buildProfiles(assignments.assignments);
    this.loaded = true;

    logger.info('ProfileLoader loaded (built-in + assignments)', undefined, LogCategory.ORCHESTRATOR);
  }

  async reload(): Promise<void> {
    this.loaded = false;
    this.profiles.clear();
    await this.load();
  }

  private buildProfiles(assignments: Record<WorkerSlot, string[]>): void {
    this.profiles.clear();

    for (const worker of ['claude', 'codex', 'gemini'] as WorkerSlot[]) {
      const persona = WORKER_PERSONAS[worker];
      if (!persona) {
        throw new Error(`缺少 Worker persona: ${worker}`);
      }

      const assignedCategories = assignments[worker] || [];

      // 从 assignedCategories 推导 strengths
      const derivedStrengths = this.deriveStrengths(assignedCategories);

      // 从未分配的高/中优先级分类推导 weaknesses
      const derivedWeaknesses = this.deriveWeaknesses(assignedCategories);

      // 创建含推导值的 persona 副本（保持 persona 其余属性不变）
      const enrichedPersona = {
        ...persona,
        strengths: derivedStrengths,
        weaknesses: derivedWeaknesses,
      };

      this.profiles.set(worker, {
        worker,
        persona: enrichedPersona,
        assignedCategories: [...assignedCategories],
      });
    }
  }

  /**
   * 从 assignedCategories 推导 strengths
   * 规则：每个分配分类的 displayName 即为一项能力
   * 过滤：排除泛化分类（simple/general），因其不具备能力区分度
   */
  private deriveStrengths(assignedCategories: string[]): string[] {
    const GENERIC_CATEGORIES = new Set(['simple', 'general']);
    return assignedCategories
      .filter(cat => !GENERIC_CATEGORIES.has(cat))
      .map(cat => CATEGORY_DEFINITIONS[cat]?.displayName)
      .filter((name): name is string => !!name);
  }

  /**
   * 从未分配的分类推导 weaknesses
   * 规则：取未分配的高/中优先级分类的 displayName，最多 3 项
   */
  private deriveWeaknesses(assignedCategories: string[]): string[] {
    const assigned = new Set(assignedCategories);
    return Object.entries(CATEGORY_DEFINITIONS)
      .filter(([cat, def]) => !assigned.has(cat) && (def.priority === 'high' || def.priority === 'medium'))
      .map(([, def]) => def.displayName)
      .slice(0, 3);
  }

  getProfile(workerType: WorkerSlot): WorkerProfile {
    const profile = this.profiles.get(workerType);
    if (!profile) {
      throw new Error(`Worker 画像未配置: ${workerType}`);
    }
    return profile;
  }

  getAllProfiles(): Map<WorkerSlot, WorkerProfile> {
    return this.profiles;
  }

  getCategory(categoryName: string): CategoryDefinition | undefined {
    return this.categories.get(categoryName);
  }

  getAllCategories(): Map<string, CategoryDefinition> {
    return this.categories;
  }

  getCategoryRules(): CategoryRules {
    return CATEGORY_RULES;
  }

  getAssignmentsForWorker(worker: WorkerSlot): string[] {
    const profile = this.getProfile(worker);
    return [...profile.assignedCategories];
  }

  getWorkerForCategory(category: string): WorkerSlot {
    return this.assignmentLoader.getWorkerForCategory(category);
  }

  getAssignmentLoader(): WorkerAssignmentLoader {
    return this.assignmentLoader;
  }
}
