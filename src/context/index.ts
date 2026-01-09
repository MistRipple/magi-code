/**
 * Context 模块导出
 * 提供上下文管理系统的所有公共接口
 */

export { MemoryDocument } from './memory-document';
export { ContextManager } from './context-manager';
export { ContextCompressor, CompressorAdapter, CompressionStats } from './context-compressor';
export { TruncationUtils, TruncationResult, truncationUtils } from './truncation-utils';
export {
  MemoryContent,
  TaskRecord,
  Decision,
  CodeChange,
  ContextMessage,
  CompressionConfig,
  TruncationConfig,
  ContextManagerConfig,
  DEFAULT_CONTEXT_CONFIG,
  DEFAULT_TRUNCATION_CONFIG,
  createEmptyMemoryContent
} from './types';

