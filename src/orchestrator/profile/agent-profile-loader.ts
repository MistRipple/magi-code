/**
 * Agent Profile Loader - 集成 LLM 配置和画像配置
 *
 * 功能：
 * - 加载 Agent 完整配置（LLM + Profile）
 * - 支持 Orchestrator 和 Worker
 * - 统一配置管理
 *
 * 设计：
 * - AgentProfile = LLM Config + Worker Guidance
 * - Orchestrator 只有 LLM 配置，无 guidance
 * - Worker 有 LLM 配置 + guidance
 */

import { logger, LogCategory } from '../../logging';
import { AgentType, AgentRole, AgentProfile, WorkerSlot, LLMConfig } from '../../types/agent-types';
import { LLMConfigLoader } from '../../llm/config';
import { ProfileLoader } from './profile-loader';
import { WorkerProfile } from './types';

/**
 * Agent Profile Loader
 * 集成 LLM 配置和画像配置
 */
export class AgentProfileLoader {
  private profileLoader: ProfileLoader;
  private profileCache: Map<AgentType, AgentProfile> = new Map();

  constructor() {
    this.profileLoader = ProfileLoader.getInstance();
  }

  /**
   * 初始化（加载所有配置）
   */
  async initialize(): Promise<void> {
    await this.profileLoader.load();
    logger.info('AgentProfileLoader initialized', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 加载完整的 Agent 配置（LLM + Profile）
   */
  loadAgentProfile(agent: AgentType): AgentProfile {
    // 检查缓存
    if (this.profileCache.has(agent)) {
      return this.profileCache.get(agent)!;
    }

    // 加载 LLM 配置
    const llmConfig = this.loadLLMConfig(agent);

    // 加载 guidance（仅 Worker 有）
    const guidance = this.loadGuidance(agent);

    // 加载高级配置
    const advanced = this.loadAdvancedConfig(agent);

    // 构建 AgentProfile
    const profile: AgentProfile = {
      agent,
      role: agent === 'orchestrator' ? 'orchestrator' : 'worker',
      llm: llmConfig,
      guidance,
      advanced,
    };

    // 缓存
    this.profileCache.set(agent, profile);

    logger.debug(`Loaded agent profile: ${agent}`, {
      hasGuidance: !!guidance,
      provider: llmConfig.provider,
      model: llmConfig.model,
    }, LogCategory.ORCHESTRATOR);

    return profile;
  }

  /**
   * 加载 LLM 配置
   */
  private loadLLMConfig(agent: AgentType): LLMConfig {
    if (agent === 'orchestrator') {
      return LLMConfigLoader.loadOrchestratorConfig();
    } else {
      const workersConfig = LLMConfigLoader.loadWorkersConfig();
      return workersConfig[agent as WorkerSlot];
    }
  }

  /**
   * 加载 Worker Guidance（仅 Worker 有）
   */
  private loadGuidance(agent: AgentType): AgentProfile['guidance'] | undefined {
    if (agent === 'orchestrator') {
      return undefined;
    }

    // 从 ProfileLoader 加载 Worker 画像
    const workerProfile = this.profileLoader.getProfile(agent as WorkerSlot);

    return {
      role: workerProfile.guidance.role,
      focus: workerProfile.guidance.focus,
      constraints: workerProfile.guidance.constraints,
    };
  }

  /**
   * 加载高级配置
   */
  private loadAdvancedConfig(agent: AgentType): AgentProfile['advanced'] | undefined {
    if (agent === 'orchestrator') {
      // Orchestrator 有特殊的高级配置
      return {
        maxTokens: 8192,
        temperature: 0.3,
      };
    }

    // Worker 使用默认配置
    return undefined;
  }

  /**
   * 获取所有 Worker 配置
   */
  getAllWorkerProfiles(): Map<WorkerSlot, AgentProfile> {
    const profiles = new Map<WorkerSlot, AgentProfile>();
    const workers: WorkerSlot[] = ['claude', 'codex', 'gemini'];

    for (const worker of workers) {
      profiles.set(worker, this.loadAgentProfile(worker));
    }

    return profiles;
  }

  /**
   * 获取 Orchestrator 配置
   */
  getOrchestratorProfile(): AgentProfile {
    return this.loadAgentProfile('orchestrator');
  }

  /**
   * 清除缓存（用于配置更新后）
   */
  clearCache(): void {
    this.profileCache.clear();
    logger.debug('Agent profile cache cleared', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 重新加载配置
   */
  async reload(): Promise<void> {
    this.clearCache();
    await this.profileLoader.reload();
    logger.info('Agent profiles reloaded', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 验证 Agent 配置
   */
  validateAgentProfile(agent: AgentType): {
    valid: boolean;
    errors: string[];
  } {
    const errors: string[] = [];

    try {
      const profile = this.loadAgentProfile(agent);

      // 验证 LLM 配置
      if (!LLMConfigLoader.validateConfig(profile.llm, agent)) {
        errors.push(`Invalid LLM configuration for ${agent}`);
      }

      // 验证 Worker guidance
      if (profile.role === 'worker' && profile.guidance) {
        if (!profile.guidance.role) {
          errors.push(`${agent}: guidance.role is required`);
        }
        if (!profile.guidance.focus || profile.guidance.focus.length === 0) {
          errors.push(`${agent}: guidance.focus must be a non-empty array`);
        }
      }

    } catch (error: any) {
      errors.push(`Failed to load profile for ${agent}: ${error.message}`);
    }

    return {
      valid: errors.length === 0,
      errors,
    };
  }

  /**
   * 验证所有配置
   */
  validateAllProfiles(): {
    valid: boolean;
    errors: string[];
  } {
    const errors: string[] = [];

    // 验证 Orchestrator
    const orchestratorResult = this.validateAgentProfile('orchestrator');
    errors.push(...orchestratorResult.errors);

    // 验证所有 Workers
    const workers: WorkerSlot[] = ['claude', 'codex', 'gemini'];
    for (const worker of workers) {
      const workerResult = this.validateAgentProfile(worker);
      errors.push(...workerResult.errors);
    }

    if (errors.length > 0) {
      logger.warn('Agent profile validation warnings', {
        errorCount: errors.length,
        errors: errors.slice(0, 5),
      }, LogCategory.ORCHESTRATOR);
    }

    return {
      valid: errors.length === 0,
      errors,
    };
  }

  /**
   * 获取底层 ProfileLoader（用于向后兼容）
   */
  getProfileLoader(): ProfileLoader {
    return this.profileLoader;
  }
}
