/**
 * CLI Arranger 主导出文件
 */

// 类型导出
export * from './types';

// 事件系统
export { EventEmitter, globalEventBus } from './events';

// 管理器
export { SessionManager } from './session-manager';
export { TaskManager } from './task-manager';
export { SnapshotManager } from './snapshot-manager';
export { DiffGenerator, DiffResult } from './diff-generator';

// CLI 检测器
export { CLIDetector, cliDetector } from './cli-detector';

// Orchestrator
export { Orchestrator, OrchestratorOptions } from './orchestrator';

// Workers
export {
  BaseWorker,
  WorkerExecuteOptions,
  ClaudeWorker,
  ClaudeWorkerConfig,
  createClaudeWorker,
  CodexWorker,
  CodexWorkerConfig,
  createCodexWorker,
  GeminiWorker,
  GeminiWorkerConfig,
  createGeminiWorker,
} from './workers';

// UI
export { WebviewProvider } from './ui/webview-provider';

// 扩展入口
export { activate, deactivate } from './extension';

