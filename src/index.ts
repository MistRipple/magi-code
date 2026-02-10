/**
 * Magi 主导出文件
 */

// 类型导出
export * from './types';

// 事件系统
export { EventEmitter, globalEventBus } from './events';

// 管理器
export { UnifiedSessionManager, UnifiedSession, SessionMessage, FileSnapshotMeta, SessionMeta, SessionStatus } from './session';
export { SnapshotManager } from './snapshot-manager';
export { DiffGenerator, DiffResult } from './diff-generator';

// UI
export { WebviewProvider } from './ui/webview-provider';

// 扩展入口
export { activate, deactivate } from './extension';
