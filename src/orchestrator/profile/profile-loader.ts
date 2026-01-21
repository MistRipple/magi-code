/**
 * Worker Profile System - 画像加载器
 *
 * 功能：
 * - 加载默认 Worker 画像配置
 * - 加载用户级配置（~/.multicli/）
 * - 合并配置（用户配置覆盖默认配置）
 * - 加载任务分类配置
 *
 * 配置文件结构（扁平化）：
 * ~/.multicli/
 * ├── config.json      - 全局配置
 * ├── categories.json  - 任务分类配置
 * ├── claude.json      - Claude Worker 画像
 * ├── codex.json       - Codex Worker 画像
 * └── gemini.json      - Gemini Worker 画像
 *
 * ⚠️ 重要：ProfileLoader 应该只有一个实例
 * - 推荐：由 MissionOrchestrator 或 MissionDrivenEngine 创建和管理
 * - 其他组件：通过依赖注入获取
 * - 避免：在多个地方创建实例
 */

import { logger, LogCategory } from '../../logging';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { WorkerSlot } from '../../types/agent-types';
import {
  WorkerProfile,
  CategoriesConfig,
  CategoryConfig,
} from './types';
import {
  DEFAULT_CLAUDE_PROFILE,
  DEFAULT_CODEX_PROFILE,
  DEFAULT_GEMINI_PROFILE,
  DEFAULT_CATEGORIES_CONFIG,
} from './defaults';
import { ProfileStorage } from './profile-storage';

/**
 * 配置验证错误
 */
export interface ConfigValidationError {
  field: string;
  message: string;
  value?: unknown;
}

/**
 * 配置验证结果
 */
export interface ConfigValidationResult {
  valid: boolean;
  errors: ConfigValidationError[];
}

export class ProfileLoader {
  private profiles: Map<WorkerSlot, WorkerProfile> = new Map();
  private categories: Map<string, CategoryConfig> = new Map();
  private categoriesConfig: CategoriesConfig;
  private loaded: boolean = false;

  /** 用户级配置目录：~/.multicli/ */
  private static readonly USER_CONFIG_DIR = path.join(os.homedir(), '.multicli');

  /** 单例实例 */
  private static instance: ProfileLoader | null = null;

  /**
   * 获取 ProfileLoader 单例实例
   * 这是获取 ProfileLoader 的唯一推荐方式
   */
  static getInstance(): ProfileLoader {
    if (!ProfileLoader.instance) {
      ProfileLoader.instance = new ProfileLoader();
    }
    return ProfileLoader.instance;
  }

  /**
   * 重置单例（仅用于测试）
   * @internal
   */
  static resetInstance(): void {
    ProfileLoader.instance = null;
  }

  /**
   * 私有构造函数，强制使用 getInstance()
   */
  private constructor() {
    this.categoriesConfig = DEFAULT_CATEGORIES_CONFIG;
  }

  /**
   * 验证 WorkerProfile 配置
   */
  private validateWorkerProfile(profile: Partial<WorkerProfile>, workerType: string): ConfigValidationResult {
    const errors: ConfigValidationError[] = [];

    // 必填字段验证
    if (!profile.name || typeof profile.name !== 'string') {
      errors.push({ field: `${workerType}.name`, message: 'name is required and must be a string', value: profile.name });
    }

    if (!profile.displayName || typeof profile.displayName !== 'string') {
      errors.push({ field: `${workerType}.displayName`, message: 'displayName is required and must be a string', value: profile.displayName });
    }

    // guidance 验证
    if (profile.guidance) {
      if (!profile.guidance.role || typeof profile.guidance.role !== 'string') {
        errors.push({ field: `${workerType}.guidance.role`, message: 'guidance.role is required and must be a string', value: profile.guidance.role });
      }

      if (!Array.isArray(profile.guidance.focus)) {
        errors.push({ field: `${workerType}.guidance.focus`, message: 'guidance.focus must be an array', value: profile.guidance.focus });
      }
    }

    // preferences 验证
    if (profile.preferences) {
      if (!Array.isArray(profile.preferences.preferredCategories)) {
        errors.push({ field: `${workerType}.preferences.preferredCategories`, message: 'must be an array', value: profile.preferences.preferredCategories });
      }
    }

    // collaboration 验证
    if (profile.collaboration) {
      if (!Array.isArray(profile.collaboration.asLeader)) {
        errors.push({ field: `${workerType}.collaboration.asLeader`, message: 'must be an array', value: profile.collaboration.asLeader });
      }
      if (!Array.isArray(profile.collaboration.asCollaborator)) {
        errors.push({ field: `${workerType}.collaboration.asCollaborator`, message: 'must be an array', value: profile.collaboration.asCollaborator });
      }
    }

    return { valid: errors.length === 0, errors };
  }

  /**
   * 验证 CategoryConfig 配置
   */
  private validateCategoryConfig(config: Partial<CategoryConfig>, categoryName: string): ConfigValidationResult {
    const errors: ConfigValidationError[] = [];

    if (!config.displayName || typeof config.displayName !== 'string') {
      errors.push({ field: `${categoryName}.displayName`, message: 'displayName is required', value: config.displayName });
    }

    if (!config.keywords || !Array.isArray(config.keywords) || config.keywords.length === 0) {
      errors.push({ field: `${categoryName}.keywords`, message: 'keywords must be a non-empty array', value: config.keywords });
    }

    if (!config.defaultWorker) {
      errors.push({ field: `${categoryName}.defaultWorker`, message: 'defaultWorker is required', value: config.defaultWorker });
    } else if (!['claude', 'codex', 'gemini'].includes(config.defaultWorker)) {
      errors.push({ field: `${categoryName}.defaultWorker`, message: 'defaultWorker must be one of: claude, codex, gemini', value: config.defaultWorker });
    }

    if (config.priority && !['high', 'medium', 'low'].includes(config.priority)) {
      errors.push({ field: `${categoryName}.priority`, message: 'priority must be one of: high, medium, low', value: config.priority });
    }

    if (config.riskLevel && !['high', 'medium', 'low'].includes(config.riskLevel)) {
      errors.push({ field: `${categoryName}.riskLevel`, message: 'riskLevel must be one of: high, medium, low', value: config.riskLevel });
    }

    return { valid: errors.length === 0, errors };
  }

  /**
   * 验证所有已加载的配置
   */
  validateAllConfigs(): ConfigValidationResult {
    const errors: ConfigValidationError[] = [];

    // 验证 Worker 画像
    for (const [workerType, profile] of this.profiles) {
      const result = this.validateWorkerProfile(profile, workerType);
      errors.push(...result.errors);
    }

    // 验证分类配置
    for (const [categoryName, config] of this.categories) {
      const result = this.validateCategoryConfig(config, categoryName);
      errors.push(...result.errors);
    }

    // 验证分类规则
    const rules = this.categoriesConfig.rules;
    if (!rules.defaultCategory || !this.categories.has(rules.defaultCategory)) {
      errors.push({
        field: 'rules.defaultCategory',
        message: 'defaultCategory must reference an existing category',
        value: rules.defaultCategory,
      });
    }

    if (errors.length > 0) {
      logger.warn('编排器.画像_加载器.配置_验证.警告', { errorCount: errors.length, errors: errors.slice(0, 5) }, LogCategory.ORCHESTRATOR);
    }

    return { valid: errors.length === 0, errors };
  }

  /**
   * 加载所有配置
   */
  async load(): Promise<void> {
    if (this.loaded) return;

    ProfileStorage.ensureDefaults({
      workers: {
        claude: DEFAULT_CLAUDE_PROFILE,
        codex: DEFAULT_CODEX_PROFILE,
        gemini: DEFAULT_GEMINI_PROFILE,
      },
      categories: DEFAULT_CATEGORIES_CONFIG,
    });

    await this.loadWorkerProfiles();
    await this.loadCategories();
    this.loaded = true;

    // 加载完成后验证配置
    this.validateAllConfigs();
  }

  /**
   * 强制重新加载配置（用于配置更新后）
   */
  async reload(): Promise<void> {
    this.loaded = false;
    this.profiles.clear();
    this.categories.clear();
    await this.load();
  }

  /**
   * 加载 Worker 画像
   * 从用户配置目录加载，覆盖默认配置
   */
  private async loadWorkerProfiles(): Promise<void> {
    const userConfigDir = ProfileLoader.USER_CONFIG_DIR;
    const defaultProfiles: Record<WorkerSlot, WorkerProfile> = {
      claude: DEFAULT_CLAUDE_PROFILE,
      codex: DEFAULT_CODEX_PROFILE,
      gemini: DEFAULT_GEMINI_PROFILE,
    };

    for (const workerType of ['claude', 'codex', 'gemini'] as WorkerSlot[]) {
      const defaultProfile = defaultProfiles[workerType];
      let finalProfile = defaultProfile;

      // 尝试加载用户配置（直接在 ~/.multicli/ 下）
      const userConfigPath = path.join(userConfigDir, `${workerType}.json`);
      if (fs.existsSync(userConfigPath)) {
        try {
          const content = fs.readFileSync(userConfigPath, 'utf-8');
          const userProfile = JSON.parse(content) as Partial<WorkerProfile>;
          finalProfile = this.mergeProfile(finalProfile, userProfile);
          logger.info('编排器.画像_加载器.子代理_配置.已加载', { workerType }, LogCategory.ORCHESTRATOR);
        } catch (error) {
          logger.warn('编排器.画像_加载器.子代理_配置.失败', { path: userConfigPath, error }, LogCategory.ORCHESTRATOR);
        }
      }

      this.profiles.set(workerType, finalProfile);
    }
  }

  /**
   * 加载任务分类配置
   * 从用户配置目录加载，覆盖默认配置
   */
  private async loadCategories(): Promise<void> {
    const userConfigPath = path.join(ProfileLoader.USER_CONFIG_DIR, 'categories.json');

    // 尝试加载用户配置
    if (fs.existsSync(userConfigPath)) {
      try {
        const content = fs.readFileSync(userConfigPath, 'utf-8');
        const userConfig = JSON.parse(content) as Partial<CategoriesConfig>;
        this.categoriesConfig = this.mergeCategoriesConfig(
          DEFAULT_CATEGORIES_CONFIG,
          userConfig
        );
        logger.info('编排器.画像_加载器.分类_配置.已加载', undefined, LogCategory.ORCHESTRATOR);
      } catch (error) {
        logger.warn('编排器.画像_加载器.分类_配置.失败', error, LogCategory.ORCHESTRATOR);
      }
    }

    // 构建分类 Map
    for (const [name, config] of Object.entries(this.categoriesConfig.categories)) {
      this.categories.set(name, config);
    }
  }

  /**
   * 合并 Worker 画像配置
   */
  private mergeProfile(
    base: WorkerProfile,
    override: Partial<WorkerProfile>
  ): WorkerProfile {
    return {
      name: override.name ?? base.name,
      displayName: override.displayName ?? base.displayName,
      version: override.version ?? base.version,
      profile: {
        ...base.profile,
        ...override.profile,
      },
      preferences: {
        preferredCategories: override.preferences?.preferredCategories ?? base.preferences.preferredCategories,
        preferredKeywords: override.preferences?.preferredKeywords ?? base.preferences.preferredKeywords,
      },
      guidance: {
        role: override.guidance?.role ?? base.guidance.role,
        focus: override.guidance?.focus ?? base.guidance.focus,
        constraints: override.guidance?.constraints ?? base.guidance.constraints,
        outputPreferences: override.guidance?.outputPreferences ?? base.guidance.outputPreferences,
      },
      collaboration: {
        asLeader: override.collaboration?.asLeader ?? base.collaboration.asLeader,
        asCollaborator: override.collaboration?.asCollaborator ?? base.collaboration.asCollaborator,
      },
    };
  }

  /**
   * 合并分类配置
   * 注意：keywords 是系统内置配置，不允许用户覆盖
   */
  private mergeCategoriesConfig(
    base: CategoriesConfig,
    override: Partial<CategoriesConfig>
  ): CategoriesConfig {
    // 深度合并 categories
    const mergedCategories: Record<string, CategoryConfig> = { ...base.categories };

    if (override.categories) {
      for (const [name, userConfig] of Object.entries(override.categories)) {
        const baseConfig = base.categories[name];
        if (baseConfig) {
          // 深度合并单个分类配置
          // keywords 始终使用系统内置配置，不允许用户覆盖
          mergedCategories[name] = {
            displayName: userConfig.displayName || baseConfig.displayName,
            description: userConfig.description || baseConfig.description,
            keywords: baseConfig.keywords, // 始终使用系统内置
            defaultWorker: userConfig.defaultWorker || baseConfig.defaultWorker,
            priority: userConfig.priority || baseConfig.priority,
            riskLevel: userConfig.riskLevel || baseConfig.riskLevel,
          };
        } else {
          // 新分类需要有 keywords，否则跳过
          if (userConfig.keywords && userConfig.keywords.length > 0) {
            mergedCategories[name] = userConfig as CategoryConfig;
          }
        }
      }
    }

    return {
      version: override.version ?? base.version,
      categories: mergedCategories,
      rules: {
        categoryPriority: override.rules?.categoryPriority ?? base.rules.categoryPriority,
        defaultCategory: override.rules?.defaultCategory ?? base.rules.defaultCategory,
        riskMapping: {
          ...base.rules.riskMapping,
          ...override.rules?.riskMapping,
        },
      },
    };
  }

  /**
   * 获取 Worker 画像
   */
  getProfile(workerType: WorkerSlot): WorkerProfile {
    return this.profiles.get(workerType) ?? DEFAULT_CLAUDE_PROFILE;
  }

  /**
   * 获取所有 Worker 画像
   */
  getAllProfiles(): Map<WorkerSlot, WorkerProfile> {
    return this.profiles;
  }

  /**
   * 获取任务分类配置
   */
  getCategory(categoryName: string): CategoryConfig | undefined {
    return this.categories.get(categoryName);
  }

  /**
   * 获取所有分类
   */
  getAllCategories(): Map<string, CategoryConfig> {
    return this.categories;
  }

  /**
   * 获取分类规则
   */
  getCategoryRules() {
    return this.categoriesConfig.rules;
  }
}
