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
 * `urlMode` 只表达路径形态；`apiProtocol` 是请求协议的唯一事实源。
 */
/** 走 HTTP 传输的连接协议：只有这三种。 */
export type HttpModelApiProtocol = 'openai_chat' | 'openai_responses' | 'anthropic_messages';

/**
 * 引擎协议标签全集。
 *
 * `chatgpt_web` **没有 HTTP 传输**：推理由内置浏览器里的 ChatGPT 网页完成，
 * 引擎条目不写 `llm`，也不带 baseUrl / apiKey / model（设计基线 A7、A22）。
 */
export type ModelApiProtocol = HttpModelApiProtocol | 'chatgpt_web';

export interface LLMConfig {
  baseUrl: string;
  urlMode: UrlMode;
  apiProtocol: HttpModelApiProtocol;
  apiKey: string;
  model: string;
  reasoningEffort?: 'low' | 'medium' | 'high' | 'xhigh';
  autonomyCapability?: ModelAutonomyCapability;
  [key: string]: unknown;
}
