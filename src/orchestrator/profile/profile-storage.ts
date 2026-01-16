/**
 * Worker Profile Storage - 画像配置持久化
 *
 * 配置存储位置：~/.multicli/config/
 * - 用户级别配置，一次配置，所有项目共享
 * - 项目目录 .multicli/ 只存储会话数据
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { WorkerProfile, CategoriesConfig } from './types';

/** 存储的配置结构 */
export interface StoredProfileConfig {
  /** Worker 画像配置 */
  workers: {
    claude?: Partial<WorkerProfile>;
    codex?: Partial<WorkerProfile>;
    gemini?: Partial<WorkerProfile>;
  };
  /** 任务分类配置 */
  categories?: Partial<CategoriesConfig>;
}

/** 配置存储管理器 */
export class ProfileStorage {
  /** 配置目录：~/.multicli/config/ */
  private static readonly CONFIG_DIR = path.join(os.homedir(), '.multicli', 'config');

  // ============================================================================
  // 配置读写
  // ============================================================================

  /**
   * 获取配置目录路径
   */
  static getConfigDir(): string {
    return ProfileStorage.CONFIG_DIR;
  }

  /**
   * 获取配置
   */
  getConfig(): StoredProfileConfig | undefined {
    const configDir = ProfileStorage.CONFIG_DIR;
    if (!fs.existsSync(configDir)) return undefined;

    const config: StoredProfileConfig = { workers: {} };
    const workersDir = path.join(configDir, 'workers');

    // 读取各 Worker 配置
    if (fs.existsSync(workersDir)) {
      for (const workerType of ['claude', 'codex', 'gemini'] as const) {
        const filePath = path.join(workersDir, `${workerType}.json`);
        if (fs.existsSync(filePath)) {
          try {
            const content = fs.readFileSync(filePath, 'utf-8');
            config.workers[workerType] = JSON.parse(content);
          } catch (e) {
            console.warn(`[ProfileStorage] 读取 ${workerType} 配置失败:`, e);
          }
        }
      }
    }

    // 读取分类配置
    const categoriesPath = path.join(configDir, 'categories.json');
    if (fs.existsSync(categoriesPath)) {
      try {
        const content = fs.readFileSync(categoriesPath, 'utf-8');
        config.categories = JSON.parse(content);
      } catch (e) {
        console.warn('[ProfileStorage] 读取分类配置失败:', e);
      }
    }

    // 检查是否有任何配置
    const hasWorkers = Object.keys(config.workers).length > 0;
    const hasCategories = config.categories !== undefined;
    if (!hasWorkers && !hasCategories) return undefined;

    return config;
  }

  /**
   * 保存配置
   */
  async saveConfig(config: StoredProfileConfig): Promise<void> {
    const configDir = ProfileStorage.CONFIG_DIR;
    const workersDir = path.join(configDir, 'workers');

    // 确保目录存在
    if (!fs.existsSync(workersDir)) {
      fs.mkdirSync(workersDir, { recursive: true });
    }

    // 保存各 Worker 配置
    for (const [workerType, workerConfig] of Object.entries(config.workers)) {
      const filePath = path.join(workersDir, `${workerType}.json`);
      if (workerConfig && Object.keys(workerConfig).length > 0) {
        fs.writeFileSync(filePath, JSON.stringify(workerConfig, null, 2), 'utf-8');
      } else if (fs.existsSync(filePath)) {
        fs.unlinkSync(filePath);
      }
    }

    // 保存分类配置
    const categoriesPath = path.join(configDir, 'categories.json');
    if (config.categories && Object.keys(config.categories).length > 0) {
      fs.writeFileSync(categoriesPath, JSON.stringify(config.categories, null, 2), 'utf-8');
    } else if (fs.existsSync(categoriesPath)) {
      fs.unlinkSync(categoriesPath);
    }
  }

  /**
   * 清除配置
   */
  async clearConfig(): Promise<void> {
    const configDir = ProfileStorage.CONFIG_DIR;
    if (fs.existsSync(configDir)) {
      fs.rmSync(configDir, { recursive: true });
    }
  }

  /**
   * 检查配置是否存在
   */
  hasConfig(): boolean {
    return this.getConfig() !== undefined;
  }
}