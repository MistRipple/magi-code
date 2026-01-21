/**
 * Config Resolver - 配置解析器
 *
 * 职责：
 * - 解析和合并配置
 * - 解析权限矩阵
 * - 解析策略配置
 * - 提供默认配置
 */

import {
  CLIType,
  PermissionMatrix,
  StrategyConfig,
} from '../types';
import { VerificationConfig } from './verification-runner';

/** 子任务计划 */
export interface SubTaskPlan {
  id: string;
  description: string;
  assignedCli: CLIType;
  reason: string;
  targetFiles?: string[];
  dependencies: string[];
  prompt: string;
}

/** 编排器配置 */
export interface OrchestratorConfig {
  timeout: number;
  /** 空闲超时时间（毫秒） */
  idleTimeout?: number;
  /** 最大执行超时时间（毫秒） */
  maxTimeout?: number;
  verification?: Partial<VerificationConfig>;
  maxRetries: number;
  integration?: {
    enabled?: boolean;
    maxRounds?: number;
    worker?: CLIType;
  };
  review?: {
    selfCheck?: boolean;
    peerReview?: 'auto' | 'always' | 'never';
    maxRounds?: number;
    highRiskExtensions?: string[];
    highRiskKeywords?: string[];
  };
  planReview?: {
    enabled?: boolean;
    reviewer?: CLIType;
  };
  permissions?: PermissionMatrix;
  strategy?: StrategyConfig;
  cliSelection?: {
    enabled?: boolean;
    healthThreshold?: number;
  };
}

const DEFAULT_CONFIG: OrchestratorConfig = {
  timeout: 300000,
  maxRetries: 3,
  integration: {
    enabled: true,
    maxRounds: 2,
    worker: 'claude',
  },
  permissions: {
    allowEdit: true,
    allowBash: true,
    allowWeb: true,
  },
  strategy: {
    enableVerification: true,
    enableRecovery: true,
    autoRollbackOnFailure: false,
  },
  cliSelection: {
    enabled: true,
    healthThreshold: 0.7,
  },
};

/**
 * 配置解析器
 */
export class ConfigResolver {
  /**
   * 解析和合并配置
   */
  static resolveConfig(config?: Partial<OrchestratorConfig>): OrchestratorConfig {
    return { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 解析权限矩阵
   */
  static resolvePermissions(config: OrchestratorConfig): PermissionMatrix {
    return {
      allowEdit: config.permissions?.allowEdit ?? true,
      allowBash: config.permissions?.allowBash ?? true,
      allowWeb: config.permissions?.allowWeb ?? true,
    };
  }

  /**
   * 解析策略配置
   */
  static resolveStrategyConfig(config: OrchestratorConfig): StrategyConfig {
    return {
      enableVerification: config.strategy?.enableVerification ?? true,
      enableRecovery: config.strategy?.enableRecovery ?? true,
      autoRollbackOnFailure: config.strategy?.autoRollbackOnFailure ?? false,
    };
  }

  /**
   * 获取默认配置
   */
  static getDefaultConfig(): OrchestratorConfig {
    return { ...DEFAULT_CONFIG };
  }
}
