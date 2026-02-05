/**
 * Context 模块导出
 * 提供上下文管理系统的所有公共接口
 */

export { MemoryDocument } from './memory-document';
export { ContextManager } from './context-manager';
export { ContextCompressor, CompressorAdapter, CompressionStats } from './context-compressor';
export { TruncationUtils, TruncationResult, truncationUtils } from './truncation-utils';

// 文件摘要缓存
export {
  FileSummaryCache,
  FileSummaryCacheEntry,
  FileSummary,
  ContextSource,
} from './file-summary-cache';

// 共享上下文池（跨 Worker 知识共享）
export {
  SharedContextPool,
  // 类型定义
  SharedContextEntryType,
  ImportanceLevel,
  FileReference,
  SharedContextEntry,
  AddResult,
  QueryOptions,
  ValidationResult,
  // 工具函数
  generateSharedContextId,
  createSharedContextEntry
} from './shared-context-pool';

// 上下文组装器（按预算分配组装上下文）
export {
  // 主类
  ContextAssembler,
  OverflowTrimmer,
  // 类型定义
  AgentContextSubscription,
  TokenBudgetConfig,
  ContextAssemblyOptions,
  ContextPartType,
  ContextPart,
  AssembledContext,
  SharedContextQueryOptions,
  ISharedContextPool,
  IFileSummaryCache,
  // 默认配置
  DEFAULT_TOKEN_BUDGET,
  DEFAULT_LOCAL_TURNS,
  // 辅助函数
  createDefaultAssemblyOptions,
  assembledContextToString,
  importanceToScore,
  meetsImportance,
} from './context-assembler';

export {
  // 核心类型
  MemoryContent,
  TaskRecord,
  Decision,
  CodeChange,
  ContextMessage,

  // 新增类型（Claude Code 对齐）
  UserMessage,
  Issue,
  ResolvedIssue,
  RejectedApproach,

  // 配置类型
  CompressionConfig,
  TruncationConfig,
  ContextManagerConfig,
  DEFAULT_CONTEXT_CONFIG,
  DEFAULT_TRUNCATION_CONFIG,
  createEmptyMemoryContent
} from './types';

