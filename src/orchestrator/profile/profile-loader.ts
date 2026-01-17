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
 * - 推荐：由 WorkerPool 创建和管理
 * - 其他组件：通过 WorkerPool.getProfileLoader() 获取
 * - 避免：在多个地方创建实例
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { WorkerType } from '../../types';
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

export class ProfileLoader {
  private profiles: Map<WorkerType, WorkerProfile> = new Map();
  private categories: Map<string, CategoryConfig> = new Map();
  private categoriesConfig: CategoriesConfig;
  private loaded: boolean = false;

  /** 用户级配置目录：~/.multicli/ */
  private static readonly USER_CONFIG_DIR = path.join(os.homedir(), '.multicli');

  /** 实例跟踪：用于检测多实例问题 */
  private static instanceCount = 0;
  private static instances: WeakRef<ProfileLoader>[] = [];
  private instanceId: number;

  constructor(_workspacePath?: string) {
    // workspacePath 参数保留用于兼容，但不再使用
    this.categoriesConfig = DEFAULT_CATEGORIES_CONFIG;

    // 实例跟踪
    ProfileLoader.instanceCount++;
    this.instanceId = ProfileLoader.instanceCount;
    ProfileLoader.instances.push(new WeakRef(this));

    // 警告：检测到多个实例
    if (ProfileLoader.instanceCount > 1) {
      const activeInstances = ProfileLoader.instances.filter(ref => ref.deref() !== undefined).length;
      console.warn(
        `[ProfileLoader] ⚠️  检测到多个 ProfileLoader 实例！` +
        `\n  当前实例 ID: ${this.instanceId}` +
        `\n  活跃实例数: ${activeInstances}` +
        `\n  总创建次数: ${ProfileLoader.instanceCount}` +
        `\n  ⚠️  这可能导致配置不一致！` +
        `\n  建议：只在 WorkerPool 中创建一个实例，其他组件通过 getProfileLoader() 获取`
      );

      // 打印堆栈跟踪以帮助定位问题
      console.warn('[ProfileLoader] 创建位置堆栈:');
      console.warn(new Error().stack);
    }
  }

  /**
   * 加载所有配置
   */
  async load(): Promise<void> {
    if (this.loaded) return;

    await this.loadWorkerProfiles();
    await this.loadCategories();
    this.loaded = true;
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
    const defaultProfiles: Record<WorkerType, WorkerProfile> = {
      claude: DEFAULT_CLAUDE_PROFILE,
      codex: DEFAULT_CODEX_PROFILE,
      gemini: DEFAULT_GEMINI_PROFILE,
    };

    for (const workerType of ['claude', 'codex', 'gemini'] as WorkerType[]) {
      const defaultProfile = defaultProfiles[workerType];
      let finalProfile = defaultProfile;

      // 尝试加载用户配置（直接在 ~/.multicli/ 下）
      const userConfigPath = path.join(userConfigDir, `${workerType}.json`);
      if (fs.existsSync(userConfigPath)) {
        try {
          const content = fs.readFileSync(userConfigPath, 'utf-8');
          const userProfile = JSON.parse(content) as Partial<WorkerProfile>;
          finalProfile = this.mergeProfile(finalProfile, userProfile);
          console.log(`[ProfileLoader] 已加载用户配置: ${workerType}`);
        } catch (error) {
          console.warn(`[ProfileLoader] 加载用户配置失败: ${userConfigPath}`, error);
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
        console.log('[ProfileLoader] 已加载用户分类配置');
      } catch (error) {
        console.warn('[ProfileLoader] 加载用户分类配置失败', error);
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
  getProfile(workerType: WorkerType): WorkerProfile {
    return this.profiles.get(workerType) ?? DEFAULT_CLAUDE_PROFILE;
  }

  /**
   * 获取所有 Worker 画像
   */
  getAllProfiles(): Map<WorkerType, WorkerProfile> {
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

