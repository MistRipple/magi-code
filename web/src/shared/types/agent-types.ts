/**
 * Agent 类型系统
 *
 * 从 magi 原始 src/types/agent-types.ts 提取的前端所需子集。
 */

/**
 * 代理角色
 */
export type AgentRole = 'orchestrator' | 'worker';

/**
 * 运行时 Agent 身份（= RoleTemplate.templateId）
 */
export type AgentId = string;

/**
 * 模型引擎 ID（用户自命名，如 'claude-main'、'gemini-fast'）
 */
export type EngineId = string;

/**
 * 系统内置 Agent（非用户配置的角色）
 */
export type SystemAgentId = 'orchestrator' | 'auxiliary';

/**
 * 全链路 Agent 身份：系统 Agent + 用户角色
 */
export type AnyAgentId = SystemAgentId | AgentId;

/**
 * 模型自治能力等级
 */
export type ModelAutonomyCapability = 'C0' | 'C1' | 'C2' | 'C3';

/**
 * URL 路径模式
 */
export type UrlMode = 'standard' | 'full';

/**
 * Token 使用统计
 */
export interface TokenUsage {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
}

/**
 * LLM 基础配置
 *
 * 注意：协议类型不再由配置字段决定，统一由 baseUrl 推断：
 *   - urlMode=standard 且 baseUrl 以 `/v1` 结尾 → OpenAI Chat Completions
 *   - urlMode=full 且 baseUrl 以 `/v1/messages` 结尾 → Anthropic Messages
 *   - 其他场景 → Anthropic Messages
 */
export interface LLMConfig {
  baseUrl: string;
  urlMode: UrlMode;
  apiKey: string;
  model: string;
  reasoningEffort?: 'low' | 'medium' | 'high' | 'xhigh';
  autonomyCapability?: ModelAutonomyCapability;
  [key: string]: unknown;
}
