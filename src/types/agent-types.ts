/**
 * Agent 类型系统
 */

/**
 * 代理角色
 */
export type AgentRole = 'orchestrator' | 'worker';

/**
 * Worker 槽位
 * 保留原有的三个槽位名称，但可配置任意 LLM
 */
export type WorkerSlot = 'claude' | 'codex' | 'gemini';

/**
 * 代理类型
 * 包含编排者和三个 Worker 槽位
 */
export type AgentType = 'orchestrator' | WorkerSlot;

/**
 * LLM 提供商
 */
export type LLMProvider = 'openai' | 'anthropic';

/**
 * 模型自治能力等级
 * - C0: 仅支持 ask + standard
 * - C1: 最高支持 ask + deep
 * - C2: 最高支持 auto + standard
 * - C3: 支持 auto + deep
 */
export type ModelAutonomyCapability = 'C0' | 'C1' | 'C2' | 'C3';

/**
 * URL 路径模式
 * - standard: 使用官方/标准代理路径规则，由系统做 /v1 级别的适配
 * - full: 用户提供完整路径，系统不再追加或裁剪版本段
 */
export type UrlMode = 'standard' | 'full';

/**
 * Token 使用统计
 */
export interface TokenUsage {
  /** 输入 token 数 */
  inputTokens: number;
  /** 输出 token 数 */
  outputTokens: number;
  /** 缓存读取 token 数 */
  cacheReadTokens?: number;
  /** 缓存写入 token 数 */
  cacheWriteTokens?: number;
}

/**
 * LLM 基础配置
 */
export interface LLMConfig {
  /** API 端点（支持代理） */
  baseUrl: string;
  /** URL 路径模式 */
  urlMode: UrlMode;
  /** API 密钥 */
  apiKey: string;
  /** 模型名称 */
  model: string;
  /** 提供商格式 */
  provider: LLMProvider;
  /** OpenAI 协议模式（仅 openai provider 生效） */
  openaiProtocol?: 'responses' | 'chat';
  /** 是否启用 */
  enabled: boolean;
  /**
   * 是否启用 extended thinking
   * - true: 强制启用
   * - false: 强制禁用
   * - undefined: 自动检测（根据模型名称）
   */
  enableThinking?: boolean;
  /**
   * 推理强度（仅对支持 reasoning 的模型生效）
   * - 'low': 低推理强度，速度快
   * - 'medium': 中等推理强度（默认）
   * - 'high': 高推理强度，质量高
   * - 'xhigh': 超高推理强度（适用于支持扩展档位的 OpenAI 兼容端点）
   */
  reasoningEffort?: 'low' | 'medium' | 'high' | 'xhigh';
  /**
   * 模型自治能力（可选）
   * - 未配置时由系统根据模型/推理强度做保守推断
   */
  autonomyCapability?: ModelAutonomyCapability;
}

/**
 * Worker 配置
 */
export interface WorkerConfig {
  /** 槽位名称 */
  slot: WorkerSlot;
  /** LLM 配置 */
  llm: LLMConfig;
  /** 画像配置 */
  profile: {
    role: string;
    focus: string[];
    constraints: string[];
  };
}

/**
 * 编排者配置
 */
export interface OrchestratorConfig {
  /** LLM 配置 */
  llm: LLMConfig;
  /** 最大 Token 数 */
  maxTokens: number;
  /** 温度参数 */
  temperature: number;
}

/**
 * 辅助模型配置
 */
export interface AuxiliaryConfig {
  /** LLM 配置 */
  llm: LLMConfig;
}

/**
 * Agent 画像
 */
export interface AgentProfile {
  /** Agent 类型 */
  agent: AgentType;
  /** Agent 角色 */
  role: AgentRole;
  /** LLM 配置 */
  llm: LLMConfig;
  /** Worker 画像（仅 Worker 有） */
  guidance?: {
    role: string;
    focus: string[];
    constraints: string[];
  };
  /** 高级配置 */
  advanced?: {
    maxTokens?: number;
    temperature?: number;
  };
}
