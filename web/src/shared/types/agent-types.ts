

/**
 * 运行时 Agent 身份（= RoleTemplate.templateId）
 */
export type AgentId = string;

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
 * LLM 基础配置
 *
 * `apiProtocol` 决定标准 API 端点和请求格式。
 */
/** 走 HTTP 传输的连接协议：只有这三种。 */
export type HttpModelApiProtocol = 'openai_chat' | 'openai_responses' | 'anthropic_messages';


export type ModelApiProtocol = HttpModelApiProtocol;

export interface LLMConfig {
  baseUrl: string;
  apiProtocol: HttpModelApiProtocol;
  apiKey: string;
  model: string;
  reasoningEffort?: import('../orchestrator-session-config').OrchestratorReasoningEffort | null;
  autonomyCapability?: ModelAutonomyCapability;
  [key: string]: unknown;
}
