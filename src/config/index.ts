/**
 * Magi 配置管理系统
 *
 * 统一管理所有配置项，支持：
 * - 环境变量覆盖
 * - 配置文件加载
 * - 默认配置
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { atomicWriteFileSync } from '../utils/atomic-write';
import { LocaleCode, DEFAULT_LOCALE } from '../i18n/types';

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
export interface OrchestratorGovernanceThresholdsConfig {
  /** 低置信度阈值：confidence < C_min => 强制人工 */
  C_min: number;
  /** 自动放行最低置信度：confidence >= C_ok */
  C_ok: number;
  /** 自动放行最高风险：risk <= R_low */
  R_low: number;
  /** 高风险强制人工阈值：risk >= R_high */
  R_high: number;
}

export interface OrchestratorConfig {
  /** 最大重试次数 */
  maxRetries: number;
  /** 默认超时 (ms) */
  defaultTimeout: number;
  /** 治理门控阈值 */
  governanceThresholds: OrchestratorGovernanceThresholdsConfig;
}

/**
 * 完整配置接口
 */
export interface MagiConfig {
  /** 界面语言 */
  locale: LocaleCode;
  context: ContextConfig;
  task: TaskConfig;
  snapshot: SnapshotConfig;
  orchestrator: OrchestratorConfig;
}

/**
 * 默认配置
 */
export const DEFAULT_CONFIG: MagiConfig = {
  locale: DEFAULT_LOCALE,
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
    maxRetries: 3,
    defaultTimeout: 300000,
    governanceThresholds: {
      C_min: 0.55,
      C_ok: 0.75,
      R_low: 0.35,
      R_high: 0.70,
    },
  },
};

/**
 * 配置管理器
 */
export class ConfigManager {
  private static instance: ConfigManager | null = null;
  private config: MagiConfig;
  private configPath: string;

  private constructor() {
    this.configPath = path.join(os.homedir(), '.magi', 'config.json');
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
  private loadConfig(): MagiConfig {
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
  private loadFromEnv(config: MagiConfig): MagiConfig {
    const result = { ...config };

    // Context
    if (process.env.MAGI_CONTEXT_MAX_TOKENS) {
      result.context.maxTokens = parseInt(process.env.MAGI_CONTEXT_MAX_TOKENS, 10);
    }
    if (process.env.MAGI_CONTEXT_ROUNDS) {
      result.context.immediateContextRounds = parseInt(process.env.MAGI_CONTEXT_ROUNDS, 10);
    }

    // Task
    if (process.env.MAGI_TASK_CACHE_SIZE) {
      result.task.maxCacheSize = parseInt(process.env.MAGI_TASK_CACHE_SIZE, 10);
    }

    // Snapshot
    if (process.env.MAGI_SNAPSHOT_CACHE_SIZE) {
      result.snapshot.maxCacheSize = parseInt(process.env.MAGI_SNAPSHOT_CACHE_SIZE, 10);
    }

    // Orchestrator governance thresholds
    const overrideThreshold = (
      key: keyof OrchestratorGovernanceThresholdsConfig,
      envKey: string,
    ): void => {
      const raw = process.env[envKey];
      if (!raw) {
        return;
      }
      const parsed = Number(raw);
      if (!Number.isFinite(parsed)) {
        console.warn(`Invalid governance threshold env ${envKey}: ${raw}`);
        return;
      }
      result.orchestrator.governanceThresholds[key] = parsed;
    };
    overrideThreshold('C_min', 'MAGI_GOV_C_MIN');
    overrideThreshold('C_ok', 'MAGI_GOV_C_OK');
    overrideThreshold('R_low', 'MAGI_GOV_R_LOW');
    overrideThreshold('R_high', 'MAGI_GOV_R_HIGH');

    return result;
  }

  /**
   * 合并配置
   */
  private mergeConfig(base: MagiConfig, override: Partial<MagiConfig>): MagiConfig {
    const overrideOrchestrator = override.orchestrator;
    const overrideGovernanceThresholds = overrideOrchestrator
      ? (overrideOrchestrator as Partial<OrchestratorConfig>).governanceThresholds
      : undefined;

    return {
      locale: override.locale ?? base.locale,
      context: { ...base.context, ...override.context },
      task: { ...base.task, ...override.task },
      snapshot: { ...base.snapshot, ...override.snapshot },
      orchestrator: {
        ...base.orchestrator,
        ...overrideOrchestrator,
        governanceThresholds: {
          ...base.orchestrator.governanceThresholds,
          ...(overrideGovernanceThresholds || {}),
        },
      },
    };
  }

  /**
   * 获取配置
   */
  get<K extends keyof MagiConfig>(key: K): MagiConfig[K] {
    return this.config[key];
  }

  /**
   * 设置配置
   */
  set<K extends keyof MagiConfig>(key: K, value: MagiConfig[K]): void {
    this.config[key] = value;
  }

  /**
   * 获取完整配置
   */
  getAll(): MagiConfig {
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
      atomicWriteFileSync(this.configPath, JSON.stringify(this.config, null, 2));
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
