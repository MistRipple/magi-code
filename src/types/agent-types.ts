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
  /** API 密钥 */
  apiKey: string;
  /** 模型名称 */
  model: string;
  /** 提供商格式 */
  provider: LLMProvider;
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
   */
  reasoningEffort?: 'low' | 'medium' | 'high';
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
 * 压缩模型配置
 */
export interface CompressorConfig {
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
