/**
 * 上下文管理系统类型定义
 *
 * 设计参考：Claude Code 上下文压缩格式
 * @see docs/context-compression-format-reference.md
 */

// Memory 文档内容结构
export interface MemoryContent {
  // ========== 元数据 ==========
  sessionId: string;
  sessionName: string;
  created: string;
  lastUpdated: string;
  tokenEstimate: number;

  // ========== 用户意图（核心）==========
  // 用户核心需求的一句话描述（对应 Claude: Primary Request and Intent）
  primaryIntent: string;
  // 用户明确的约束条件（如"不要打补丁"、"自然语言优先"）
  userConstraints: string[];
  // 用户关键原话（保留原文，便于后续 session 准确理解意图）
  userMessages: UserMessage[];

  // ========== 任务状态 ==========
  // 进行中的任务（对应 Claude: Pending Tasks）
  currentTasks: TaskRecord[];
  // 已完成的任务
  completedTasks: TaskRecord[];
  // 当前正在做什么（对应 Claude: Current Work）
  currentWork: string;
  // 下一步建议（对应 Claude: Optional Next Step）
  nextSteps: string[];

  // ========== 技术上下文 ==========
  // 关键技术决策及原因（对应 Claude: Key Technical Concepts 中的决策部分）
  keyDecisions: Decision[];
  // 代码变更记录（对应 Claude: Files and Code Sections）
  codeChanges: CodeChange[];
  // 重要上下文信息（技术概念、模块说明等）
  importantContext: string[];

  // ========== 问题跟踪 ==========
  // 待解决问题
  pendingIssues: Issue[];
  // 已解决问题及方案（对应 Claude: Errors and fixes）
  resolvedIssues: ResolvedIssue[];
  // 被拒绝的方案及原因（对应 Claude: Problem Solving 中的否决记录）
  rejectedApproaches: RejectedApproach[];
}

// 用户消息记录（保留原话）
export interface UserMessage {
  // 消息原文
  content: string;
  // 消息时间
  timestamp: string;
  // 是否为关键指令（如决策、约束、确认）
  isKeyInstruction: boolean;
}

// 待解决问题（升级自 string）
export interface Issue {
  id: string;
  description: string;
  // 问题来源：用户反馈 / 系统检测 / AI 发现
  source: 'user' | 'system' | 'ai';
  timestamp: string;
}

// 已解决问题
export interface ResolvedIssue {
  id: string;
  // 问题描述
  problem: string;
  // 根因分析
  rootCause: string;
  // 解决方案
  solution: string;
  timestamp: string;
}

// 被拒绝的方案
export interface RejectedApproach {
  id: string;
  // 方案描述
  approach: string;
  // 拒绝原因
  reason: string;
  // 拒绝来源：用户明确拒绝 / 技术不可行
  rejectedBy: 'user' | 'technical';
  timestamp: string;
}

// 任务记录
export interface TaskRecord {
  id: string;
  description: string;
  status: 'pending' | 'in_progress' | 'completed' | 'failed';
  assignedWorker?: string;
  result?: string;
  timestamp: string;
}

// 关键决策
export interface Decision {
  id: string;
  description: string;
  reason: string;
  timestamp: string;
}

// 代码变更记录
export interface CodeChange {
  file: string;
  action: 'add' | 'modify' | 'delete';
  summary: string;
  timestamp: string;
}

// 即时上下文消息
export interface ContextMessage {
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: string;
  tokenCount?: number;
}

// 截断配置（Augment 风格）
export interface TruncationConfig {
  // 单条消息最大字符数
  maxMessageChars: number;
  // 工具输出最大字符数
  maxToolOutputChars: number;
  // 截断提示信息
  truncationNotice: string;
  // 是否启用截断
  enabled: boolean;
}

// 压缩配置
export interface CompressionConfig {
  // Token 上限
  tokenLimit: number;
  // 行数上限
  lineLimit: number;
  // 压缩目标比例
  compressionRatio: number;
  // 保留优先级
  retentionPriority: (keyof MemoryContent)[];
  // 截断配置
  truncation: TruncationConfig;
}

// 上下文管理器配置
export interface ContextManagerConfig {
  // 会话存储路径
  storagePath: string;
  // 即时上下文保留轮数
  immediateContextRounds: number;
  // 压缩配置
  compression: CompressionConfig;
  // 是否启用项目知识库
  enableKnowledgeBase: boolean;
}

// 默认截断配置（基于 Augment 的实践）
// Augment 使用 50000 字符作为响应截断限制
export const DEFAULT_TRUNCATION_CONFIG: TruncationConfig = {
  maxMessageChars: 50000,      // 单条消息最大 50K 字符（与 Augment 一致）
  maxToolOutputChars: 50000,   // 工具输出最大 50K 字符（与 Augment 一致）
  truncationNotice: '<response clipped><NOTE>To save on context only part of this content has been shown.</NOTE>',
  enabled: true
};

// 默认配置
export const DEFAULT_CONTEXT_CONFIG: ContextManagerConfig = {
  storagePath: '.multicli/sessions',
  immediateContextRounds: 5,
  compression: {
    tokenLimit: 8000,
    lineLimit: 200,
    compressionRatio: 0.5,
    // 保留优先级（按重要性排序）
    retentionPriority: [
      'primaryIntent',       // 🔴 最重要：用户核心意图
      'userConstraints',     // 🔴 重要：用户约束条件
      'currentTasks',        // 🔴 重要：进行中的任务
      'currentWork',         // 🟡 中等：当前工作状态
      'keyDecisions',        // 🟡 中等：关键决策
      'userMessages',        // 🟡 中等：用户原话
      'nextSteps',           // 🟡 中等：下一步建议
      'rejectedApproaches',  // 🟡 中等：被拒绝的方案
      'importantContext',    // 🟢 次要：重要上下文
      'codeChanges',         // 🟢 次要：代码变更
      'pendingIssues',       // 🟢 次要：待解决问题
      'resolvedIssues',      // 🟢 次要：已解决问题
      'completedTasks'       // 🟢 最后：已完成任务（可压缩）
    ],
    truncation: DEFAULT_TRUNCATION_CONFIG
  },
  enableKnowledgeBase: false
};

// 创建空的 Memory 内容
export function createEmptyMemoryContent(sessionId: string, sessionName: string): MemoryContent {
  const now = new Date().toISOString();
  return {
    // 元数据
    sessionId,
    sessionName,
    created: now,
    lastUpdated: now,
    tokenEstimate: 0,

    // 用户意图（核心）
    primaryIntent: '',
    userConstraints: [],
    userMessages: [],

    // 任务状态
    currentTasks: [],
    completedTasks: [],
    currentWork: '',
    nextSteps: [],

    // 技术上下文
    keyDecisions: [],
    codeChanges: [],
    importantContext: [],

    // 问题跟踪
    pendingIssues: [],
    resolvedIssues: [],
    rejectedApproaches: []
  };
}