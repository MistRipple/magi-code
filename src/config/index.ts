/**
 * MultiCLI 配置管理系统
 *
 * 统一管理所有配置项，支持：
 * - 环境变量覆盖
 * - 配置文件加载
 * - 默认配置
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

/**
 * 上下文配置
 */
export interface ContextConfig {
  /** 最大 Token 数 */
  maxTokens: number;
  /** 即时上下文轮数 */
  immediateContextRounds: number;
  /** Memory Token 限制 */
  memoryTokenLimit: number;
  /** 是否启用截断 */
  truncationEnabled: boolean;
  /** 截断快照的最大字符数 */
  snapshotMaxChars: number;
}

/**
 * 任务系统配置
 */
export interface TaskConfig {
  /** 缓存最大大小 */
  maxCacheSize: number;
  /** 超时检查间隔 (ms) */
  timeoutCheckInterval: number;
  /** 默认优先级 */
  defaultPriority: number;
}

/**
 * 快照系统配置
 */
export interface SnapshotConfig {
  /** 缓存最大大小 */
  maxCacheSize: number;
  /** 原子操作超时 (ms) */
  atomicOperationTimeout: number;
  /** 是否启用文件锁 */
  enableFileLock: boolean;
  /** 锁超时时间 (ms) */
  lockTimeout: number;
}

/**
 * 编排器配置
 */
export interface OrchestratorConfig {
  /** 计划确认阈值 */
  planConfirmationThreshold: 'low' | 'medium' | 'high';
  /** 最大重试次数 */
  maxRetries: number;
  /** 默认超时 (ms) */
  defaultTimeout: number;
}

/**
 * 性能配置
 */
export interface PerformanceConfig {
  /** 是否启用精确 Token 计算 */
  enablePreciseTokenCounting: boolean;
  /** 是否启用分布式锁 */
  enableDistributedLock: boolean;
  /** 是否启用性能监控 */
  enablePerformanceMonitoring: boolean;
}

/**
 * 完整配置接口
 */
export interface MultiCLIConfig {
  context: ContextConfig;
  task: TaskConfig;
  snapshot: SnapshotConfig;
  orchestrator: OrchestratorConfig;
  performance: PerformanceConfig;
}

/**
 * 默认配置
 */
export const DEFAULT_CONFIG: MultiCLIConfig = {
  context: {
    maxTokens: 8000,
    immediateContextRounds: 5,
    memoryTokenLimit: 8000,
    truncationEnabled: true,
    snapshotMaxChars: 6000,
  },
  task: {
    maxCacheSize: 1000,
    timeoutCheckInterval: 5000,
    defaultPriority: 5,
  },
  snapshot: {
    maxCacheSize: 100,
    atomicOperationTimeout: 30000,
    enableFileLock: true,
    lockTimeout: 60000,
  },
  orchestrator: {
    planConfirmationThreshold: 'medium',
    maxRetries: 3,
    defaultTimeout: 300000,
  },
  performance: {
    enablePreciseTokenCounting: true,
    enableDistributedLock: false,
    enablePerformanceMonitoring: true,
  },
};

/**
 * 配置管理器
 */
export class ConfigManager {
  private static instance: ConfigManager | null = null;
  private config: MultiCLIConfig;
  private configPath: string;

  private constructor() {
    this.configPath = path.join(os.homedir(), '.multicli', 'config.json');
    this.config = this.loadConfig();
  }

  /**
   * 获取单例实例
   */
  static getInstance(): ConfigManager {
    if (!ConfigManager.instance) {
      ConfigManager.instance = new ConfigManager();
    }
    return ConfigManager.instance;
  }

  /**
   * 重置单例（仅用于测试）
   */
  static resetInstance(): void {
    ConfigManager.instance = null;
  }

  /**
   * 加载配置
   */
  private loadConfig(): MultiCLIConfig {
    let config = { ...DEFAULT_CONFIG };

    // 1. 尝试从配置文件加载
    if (fs.existsSync(this.configPath)) {
      try {
        const fileConfig = JSON.parse(fs.readFileSync(this.configPath, 'utf-8'));
        config = this.mergeConfig(config, fileConfig);
      } catch (error) {
        console.warn(`Failed to load config from ${this.configPath}:`, error);
      }
    }

    // 2. 从环境变量覆盖
    config = this.loadFromEnv(config);

    return config;
  }

  /**
   * 从环境变量加载配置
   */
  private loadFromEnv(config: MultiCLIConfig): MultiCLIConfig {
    const result = { ...config };

    // Context
    if (process.env.MULTICLI_CONTEXT_MAX_TOKENS) {
      result.context.maxTokens = parseInt(process.env.MULTICLI_CONTEXT_MAX_TOKENS, 10);
    }
    if (process.env.MULTICLI_CONTEXT_ROUNDS) {
      result.context.immediateContextRounds = parseInt(process.env.MULTICLI_CONTEXT_ROUNDS, 10);
    }

    // Task
    if (process.env.MULTICLI_TASK_CACHE_SIZE) {
      result.task.maxCacheSize = parseInt(process.env.MULTICLI_TASK_CACHE_SIZE, 10);
    }

    // Snapshot
    if (process.env.MULTICLI_SNAPSHOT_CACHE_SIZE) {
      result.snapshot.maxCacheSize = parseInt(process.env.MULTICLI_SNAPSHOT_CACHE_SIZE, 10);
    }

    // Performance
    if (process.env.MULTICLI_PRECISE_TOKENS) {
      result.performance.enablePreciseTokenCounting = process.env.MULTICLI_PRECISE_TOKENS === 'true';
    }

    return result;
  }

  /**
   * 合并配置
   */
  private mergeConfig(base: MultiCLIConfig, override: Partial<MultiCLIConfig>): MultiCLIConfig {
    return {
      context: { ...base.context, ...override.context },
      task: { ...base.task, ...override.task },
      snapshot: { ...base.snapshot, ...override.snapshot },
      orchestrator: { ...base.orchestrator, ...override.orchestrator },
      performance: { ...base.performance, ...override.performance },
    };
  }

  /**
   * 获取配置
   */
  get<K extends keyof MultiCLIConfig>(key: K): MultiCLIConfig[K] {
    return this.config[key];
  }

  /**
   * 设置配置
   */
  set<K extends keyof MultiCLIConfig>(key: K, value: MultiCLIConfig[K]): void {
    this.config[key] = value;
  }

  /**
   * 获取完整配置
   */
  getAll(): MultiCLIConfig {
    return { ...this.config };
  }

  /**
   * 保存配置到文件
   */
  save(): void {
    try {
      const dir = path.dirname(this.configPath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      fs.writeFileSync(this.configPath, JSON.stringify(this.config, null, 2), 'utf-8');
    } catch (error) {
      console.error('Failed to save config:', error);
    }
  }

  /**
   * 重置为默认配置
   */
  reset(): void {
    this.config = { ...DEFAULT_CONFIG };
  }
}
