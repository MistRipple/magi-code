/**
 * Worker Profile System - 画像加载器（单一数据源版）
 *
 * 仅使用内置 personas/category definitions + worker-assignments.json。
 * 不读取任何旧配置文件。
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
      this.profiles.set(worker, {
        worker,
        persona,
        assignedCategories: [...assignedCategories],
      });
    }
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
