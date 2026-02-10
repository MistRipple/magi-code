/**
 * Worker assignments loader (single source of truth)
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { logger, LogCategory } from '../../logging';
import { WorkerSlot } from '../../types/agent-types';
import { WorkerAssignments } from './types';
import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';
import { DEFAULT_ASSIGNMENTS, WORKER_ASSIGNMENTS_VERSION } from './builtin/default-assignments';

const WORKERS: WorkerSlot[] = ['claude', 'codex', 'gemini'];

export class WorkerAssignmentStorage {
  private static readonly CONFIG_DIR = path.join(os.homedir(), '.magi');
  private static readonly ASSIGNMENTS_FILE = path.join(this.CONFIG_DIR, 'worker-assignments.json');

  static getConfigDir(): string {
    return this.CONFIG_DIR;
  }

  static getConfigPath(): string {
    return this.ASSIGNMENTS_FILE;
  }

  static buildDefault(): WorkerAssignments {
    return {
      version: WORKER_ASSIGNMENTS_VERSION,
      assignments: {
        claude: [...DEFAULT_ASSIGNMENTS.claude],
        codex: [...DEFAULT_ASSIGNMENTS.codex],
        gemini: [...DEFAULT_ASSIGNMENTS.gemini],
      },
    };
  }

  static ensureDefaults(): WorkerAssignments {
    if (!fs.existsSync(this.ASSIGNMENTS_FILE)) {
      const defaults = this.buildDefault();
      this.save(defaults);
      return defaults;
    }

    return this.load();
  }

  static load(): WorkerAssignments {
    if (!fs.existsSync(this.ASSIGNMENTS_FILE)) {
      return this.ensureDefaults();
    }

    const content = fs.readFileSync(this.ASSIGNMENTS_FILE, 'utf-8');
    const parsed = JSON.parse(content) as WorkerAssignments;
    this.validate(parsed);
    return parsed;
  }

  static save(assignments: WorkerAssignments): void {
    this.validate(assignments);

    if (!fs.existsSync(this.CONFIG_DIR)) {
      fs.mkdirSync(this.CONFIG_DIR, { recursive: true });
    }

    fs.writeFileSync(this.ASSIGNMENTS_FILE, JSON.stringify(assignments, null, 2), 'utf-8');
    logger.info('Worker assignments saved', { path: this.ASSIGNMENTS_FILE }, LogCategory.ORCHESTRATOR);
  }

  static validate(assignments: WorkerAssignments): void {
    const errors: string[] = [];

    if (!assignments || typeof assignments !== 'object') {
      throw new Error('worker-assignments.json 格式无效');
    }

    if (assignments.version !== WORKER_ASSIGNMENTS_VERSION) {
      errors.push(`版本不匹配: ${assignments.version || 'unknown'} (期望 ${WORKER_ASSIGNMENTS_VERSION})`);
    }

    if (!assignments.assignments || typeof assignments.assignments !== 'object') {
      errors.push('assignments 字段缺失或格式错误');
    }

    const assignmentMap = assignments.assignments || ({} as WorkerAssignments['assignments']);
    const seenCategories = new Map<string, WorkerSlot>();
    const definedCategories = new Set(Object.keys(CATEGORY_DEFINITIONS));

    for (const worker of WORKERS) {
      const categories = assignmentMap[worker];
      if (!Array.isArray(categories)) {
        errors.push(`Worker ${worker} 的 assignments 必须是数组`);
        continue;
      }

      for (const category of categories) {
        if (typeof category !== 'string' || category.trim().length === 0) {
          errors.push(`Worker ${worker} 的分类包含非法值`);
          continue;
        }

        if (!definedCategories.has(category)) {
          errors.push(`未知分类: ${category}`);
          continue;
        }

        if (seenCategories.has(category)) {
          errors.push(`分类重复归属: ${category}`);
          continue;
        }

        seenCategories.set(category, worker);
      }
    }

    const missingCategories = Array.from(definedCategories).filter(c => !seenCategories.has(c));
    if (missingCategories.length > 0) {
      errors.push(`分类未归属: ${missingCategories.join(', ')}`);
    }

    if (errors.length > 0) {
      const message = `worker-assignments.json 配置非法: ${errors.join('；')}`;
      logger.error(message, undefined, LogCategory.ORCHESTRATOR);
      throw new Error(message);
    }
  }
}

export class WorkerAssignmentLoader {
  private loaded = false;
  private assignments: WorkerAssignments | null = null;

  load(): WorkerAssignments {
    if (!this.loaded || !this.assignments) {
      this.assignments = WorkerAssignmentStorage.ensureDefaults();
      this.loaded = true;
    }
    return this.assignments;
  }

  reload(): WorkerAssignments {
    this.assignments = WorkerAssignmentStorage.load();
    this.loaded = true;
    return this.assignments;
  }

  getAssignments(): WorkerAssignments {
    return this.load();
  }

  getCategoriesForWorker(worker: WorkerSlot): string[] {
    const data = this.load();
    const categories = data.assignments[worker];
    if (!categories) {
      throw new Error(`未知 Worker: ${worker}`);
    }
    return [...categories];
  }

  getWorkerForCategory(category: string): WorkerSlot {
    const data = this.load();

    for (const worker of WORKERS) {
      if (data.assignments[worker]?.includes(category)) {
        return worker;
      }
    }

    throw new Error(`分类未归属: ${category}`);
  }

  getCategoryMap(): Record<string, WorkerSlot> {
    const data = this.load();
    const mapping: Record<string, WorkerSlot> = {};

    for (const worker of WORKERS) {
      for (const category of data.assignments[worker] || []) {
        mapping[category] = worker;
      }
    }

    return mapping;
  }
}
