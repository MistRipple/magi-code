/**
 * 消息类型定义
 */

// 消息角色
export type MessageRole = 'user' | 'assistant' | 'system';

// 消息来源
export type MessageSource = 'orchestrator' | 'claude' | 'codex' | 'gemini' | 'system';

// 消息类型
export type MessageType = 'message' | 'system-notice' | 'tool_call' | 'plan_confirmation' | 'question_request';

// 通知类型
export type NoticeType = 'info' | 'success' | 'warning' | 'error';

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
  id?: string;                // 唯一标识符，用于 #each 循环的 key
  type: 'text' | 'code' | 'thinking' | 'tool_call' | 'tool_result' | 'file_change' | 'plan';
  content: string;
  language?: string;        // 代码块语言
  toolCall?: ToolCall;      // 工具调用信息
  thinking?: ThinkingBlock; // 思考块信息
  fileChange?: {
    filePath: string;
    changeType: 'create' | 'modify' | 'delete';
    additions?: number;
    deletions?: number;
    diff?: string;
  };
  plan?: {
    goal: string;
    analysis?: string;
    constraints?: string[];
    acceptanceCriteria?: string[];
    riskLevel?: 'low' | 'medium' | 'high';
    riskFactors?: string[];
    rawJson?: string;
  };
}

// Worker Todo
export interface AssignmentTodo {
  id: string;
  assignmentId: string;
  content: string;
  reasoning?: string;
  expectedOutput?: string;
  type: string;
  priority: number;
  status: string;
  outOfScope?: boolean;
  approvalStatus?: 'pending' | 'approved' | 'rejected';
  approvalNote?: string;
}

// Assignment 规划
export interface AssignmentPlan {
  id: string;
  workerId: string;
  responsibility: string;
  status?: string;
  progress?: number;
  todos: AssignmentTodo[];
}

// Mission 规划
export interface MissionPlan {
  missionId: string;
  assignments: AssignmentPlan[];
}

// Wave 执行状态（提案 4.6）
export interface WaveState {
  /** 当前 Wave 索引 */
  currentWave: number;
  /** 总 Wave 数 */
  totalWaves: number;
  /** 每个 Wave 的任务 ID */
  waves: string[][];
  /** 关键路径 */
  criticalPath: string[];
  /** Wave 执行状态 */
  status: 'idle' | 'executing' | 'completed';
}

// Worker Session 状态（提案 4.1）
export interface WorkerSessionState {
  /** Session ID */
  sessionId: string;
  /** Assignment ID */
  assignmentId: string;
  /** Worker ID */
  workerId: string;
  /** 是否为恢复的 Session */
  isResumed: boolean;
  /** 已完成的 Todo 数 */
  completedTodos: number;
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
  type?: MessageType;         // 消息类型（notice = 系统通知）
  noticeType?: NoticeType;    // 通知类型（info/success/warning/error）
  metadata?: {
    model?: string;
    tokens?: number;
    duration?: number;
    worker?: string;        // Worker 类型（orchestrator, coder, reviewer 等）
    filePath?: string;      // 相关文件路径
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
  name?: string;  // 可选，未命名会话可能没有 name
  createdAt: number;
  updatedAt: number;
  messageCount?: number;
  preview?: string;  // 会话预览
  messages?: { id: string; role: string; content: string }[];
}

// 处理中的 Actor
export interface ProcessingActor {
  source: MessageSource;
  agent: AgentType;
}

// Tab 类型
export type TabType = 'thread' | 'claude' | 'codex' | 'gemini' | 'settings' | 'knowledge' | 'tasks' | 'edits';

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

// 任务状态
export type TaskStatus = 'pending' | 'paused' | 'running' | 'completed' | 'failed' | 'cancelled';

// 任务
export interface Task {
  id?: string;
  name?: string;
  prompt?: string;
  description?: string;
  status: TaskStatus;
}

// 编辑/变更记录
export type EditType = 'add' | 'modify' | 'delete';

export interface Edit {
  filePath: string;
  type?: EditType;
  additions?: number;
  deletions?: number;
  contributors?: string[];
  workerId?: string;
}

// Toast 通知
export type ToastType = 'info' | 'success' | 'warning' | 'error';

export interface Toast {
  id: string;
  type: ToastType;
  title?: string;
  message: string;
}

// 应用状态（后端下发的完整状态）
export interface AppState {
  sessions?: Session[];
  currentSession?: Session;
  isProcessing?: boolean;
  pendingChanges?: unknown[];
  tasks?: Task[];
  edits?: Edit[];
  toasts?: Toast[];
  interactionMode?: 'ask' | 'auto';
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
