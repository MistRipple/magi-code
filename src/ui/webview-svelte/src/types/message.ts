/**
 * 消息类型定义
 */

// 消息角色
export type MessageRole = 'user' | 'assistant' | 'system';

// 占位消息状态（符合 message-response-flow-design.md 规范）
export type PlaceholderState =
  | 'pending'    // 正在准备...（发送后立即）
  | 'received'   // 已接收...（后端确认接收）
  | 'thinking'   // 正在思考...（编排进入分析）
  ;

// 请求-响应绑定
export interface RequestResponseBinding {
  /** 用户请求 ID（前端生成） */
  requestId: string;
  /** 用户消息 ID */
  userMessageId: string;
  /** 占位消息 ID（前端生成） */
  placeholderMessageId: string;
  /** 真实响应消息 ID（后端生成） */
  realMessageId?: string;
  /** 创建时间戳 */
  createdAt: number;
  /** 首 token 超时定时器 ID */
  timeoutId?: ReturnType<typeof setTimeout>;
}

// 消息来源
export type MessageSource = 'orchestrator' | 'claude' | 'codex' | 'gemini' | 'system';

// 消息类型 - 与协议层 MessageType 完全对齐
export type MessageType =
  // 协议层核心类型
  | 'text'
  | 'plan'
  | 'progress'
  | 'result'
  | 'error'
  | 'interaction'
  | 'system-notice'
  | 'tool_call'
  | 'thinking'
  // 方案 B 扩展类型
  | 'user_input'
  | 'task_card'
  | 'instruction';

// 通知类型
export type NoticeType = 'info' | 'success' | 'warning' | 'error';

// 工具调用状态
export type ToolCallStatus = 'pending' | 'running' | 'success' | 'error';

// 工具结果标准化状态（与协议层保持一致）
export type StandardizedToolStatus =
  | 'success'
  | 'error'
  | 'timeout'
  | 'killed'
  | 'blocked'
  | 'rejected'
  | 'aborted';

// 工具结果标准化结构（机器可读）
export interface StandardizedToolResult {
  schemaVersion: 'tool-result.v1';
  source: 'builtin' | 'mcp' | 'skill';
  toolName: string;
  toolCallId: string;
  status: StandardizedToolStatus;
  message: string;
  data?: unknown;
  errorCode?: string;
  sourceId?: string;
}

// 工具调用
export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  status: ToolCallStatus;
  result?: string;
  error?: string;
  standardized?: StandardizedToolResult;
  startTime?: number;
  endTime?: number;
}

// 思考块
export interface ThinkingBlock {
  content: string;
  isComplete: boolean;
  summary?: string;
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
  parentId?: string;
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

// 模型连接状态类型（统一的连接状态，供 BottomTabs 和 SettingsPanel 共用）
export type ModelStatusType =
  | 'available'       // 可用（已连接）
  | 'connected'       // 已连接
  | 'disabled'        // 已禁用
  | 'not_configured'  // 未配置
  | 'checking'        // 检测中
  | 'error'           // 错误
  | 'unavailable'     // 不可用
  | 'invalid_model'   // 无效模型
  | 'auth_failed'     // 认证失败
  | 'network_error'   // 网络错误
  | 'timeout'         // 超时
  | 'orchestrator';  // 使用编排者模型

export interface ModelStatus {
  status: ModelStatusType;
  model?: string;
  version?: string;
  tokens?: number;
  error?: string;
}

// 模型状态映射
export type ModelStatusMap = Record<string, ModelStatus>;

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
  /** 用户上传的图片（base64 Data URL 格式） */
  images?: Array<{ dataUrl: string }>;
  metadata?: {
    model?: string;
    tokens?: number;
    duration?: number;
    worker?: string;        // Worker 类型（orchestrator, coder, reviewer 等）
    filePath?: string;      // 相关文件路径
    // 占位消息相关字段
    isPlaceholder?: boolean;          // 是否为占位消息
    placeholderState?: PlaceholderState; // 占位消息状态
    requestId?: string;               // 关联的请求 ID
    wasPlaceholder?: boolean;         // 是否从占位消息转换而来（用于过渡动画）
    justCompleted?: boolean;          // 是否刚完成（用于完成动画）
    sendingAnimation?: boolean;       // 用户消息发送动画
    eventId?: string;                 // 事件 ID（后端下发）
    eventSeq?: number;                // 事件序号（会话内单调递增）
    cardId?: string;                  // 卡片实体 ID
    cardStreamSeq?: number;           // 卡片流式序号
    parentCardId?: string;            // 父卡片 ID（补遗卡片）
    finalStreamSeq?: number;          // 封口流式序号
    lateArrival?: boolean;            // 是否为晚到补遗
    lateFromCardId?: string;          // 晚到来源 cardId
    images?: Array<{ dataUrl: string }>; // 🔧 从 metadata 提取的图片（后端传递）
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

// 子任务状态（对齐后端 SubTaskViewStatus）
export type SubTaskStatus =
  | 'pending'
  | 'running'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'skipped'
  | 'blocked'
  | 'cancelled'
  | 'in_progress'; // 增量事件可能发送此值

// 子任务（对齐后端 TodoItemView）
export interface SubTaskItem {
  id: string;
  description: string;
  title?: string;
  assignedWorker: string;
  assignmentId: string;
  status: SubTaskStatus;
  progress: number;
  priority: number;
  targetFiles: string[];
  modifiedFiles?: string[];
  error?: string;
  startedAt?: number;
  completedAt?: number;
}

// 任务（对齐后端 TaskView）
export interface Task {
  id: string;
  name: string;
  prompt?: string;
  description?: string;
  status: TaskStatus;
  subTasks: SubTaskItem[];
  progress: number;
  missionId: string;
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
  interactionModeUpdatedAt?: number;
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
