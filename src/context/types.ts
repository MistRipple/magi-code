/**
 * 上下文管理系统类型定义
 */

// Memory 文档内容结构
export interface MemoryContent {
  // 元数据
  sessionId: string;
  sessionName: string;
  created: string;
  lastUpdated: string;
  tokenEstimate: number;

  // 任务相关
  currentTasks: TaskRecord[];
  completedTasks: TaskRecord[];

  // 决策和变更
  keyDecisions: Decision[];
  codeChanges: CodeChange[];

  // 上下文信息
  importantContext: string[];
  pendingIssues: string[];
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
  storagePath: '.cli-arranger/sessions',
  immediateContextRounds: 5,
  compression: {
    tokenLimit: 8000,
    lineLimit: 200,
    compressionRatio: 0.5,
    retentionPriority: [
      'currentTasks',
      'keyDecisions',
      'importantContext',
      'codeChanges',
      'completedTasks',
      'pendingIssues'
    ],
    truncation: DEFAULT_TRUNCATION_CONFIG
  },
  enableKnowledgeBase: false
};

// 创建空的 Memory 内容
export function createEmptyMemoryContent(sessionId: string, sessionName: string): MemoryContent {
  const now = new Date().toISOString();
  return {
    sessionId,
    sessionName,
    created: now,
    lastUpdated: now,
    tokenEstimate: 0,
    currentTasks: [],
    completedTasks: [],
    keyDecisions: [],
    codeChanges: [],
    importantContext: [],
    pendingIssues: []
  };
}

