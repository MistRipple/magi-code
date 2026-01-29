/**
 * 消息类型定义
 */

// 消息角色
export type MessageRole = 'user' | 'assistant' | 'system';

// 消息来源
export type MessageSource = 'orchestrator' | 'claude' | 'codex' | 'gemini';

// 工具调用状态
export type ToolCallStatus = 'pending' | 'running' | 'success' | 'error';

// 工具调用
export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  status: ToolCallStatus;
  result?: string;
  error?: string;
  startTime?: number;
  endTime?: number;
}

// 思考块
export interface ThinkingBlock {
  content: string;
  isComplete: boolean;
}

// 消息内容块
export interface ContentBlock {
  type: 'text' | 'code' | 'thinking' | 'tool_call' | 'tool_result';
  content: string;
  language?: string;        // 代码块语言
  toolCall?: ToolCall;      // 工具调用信息
  thinking?: ThinkingBlock; // 思考块信息
}

// 单条消息
export interface Message {
  id: string;
  role: MessageRole;
  source: MessageSource;
  content: string;            // 完整内容（用于 Markdown 渲染）
  blocks?: ContentBlock[];    // 结构化内容块
  timestamp: number;
  isStreaming: boolean;       // 是否正在流式输出
  isComplete: boolean;        // 是否已完成
  metadata?: {
    model?: string;
    tokens?: number;
    duration?: number;
    [key: string]: unknown;
  };
}

// Agent 类型
export type AgentType = 'claude' | 'codex' | 'gemini';

// Agent 输出
export interface AgentOutputs {
  claude: Message[];
  codex: Message[];
  gemini: Message[];
}

// 会话信息
export interface Session {
  id: string;
  name: string;
  createdAt: number;
  updatedAt: number;
  messageCount: number;
}

// 处理中的 Actor
export interface ProcessingActor {
  source: MessageSource;
  agent: AgentType;
}

// Tab 类型
export type TabType = 'thread' | 'claude' | 'codex' | 'gemini' | 'settings' | 'knowledge';

// 滚动位置映射
export interface ScrollPositions {
  thread: number;
  claude: number;
  codex: number;
  gemini: number;
}

// 自动滚动配置
export interface AutoScrollConfig {
  thread: boolean;
  claude: boolean;
  codex: boolean;
  gemini: boolean;
}

// 应用状态（后端下发的完整状态）
export interface AppState {
  sessions?: Session[];
  currentSession?: Session;
  isProcessing?: boolean;
  pendingChanges?: unknown[];
  tasks?: unknown[];
  [key: string]: unknown;
}

// Webview 持久化状态
export interface WebviewPersistedState {
  currentTopTab: TabType;
  currentBottomTab: TabType;
  threadMessages: Message[];
  agentOutputs: AgentOutputs;
  sessions: Session[];
  currentSessionId: string | null;
  scrollPositions: ScrollPositions;
  autoScrollEnabled: AutoScrollConfig;
}

