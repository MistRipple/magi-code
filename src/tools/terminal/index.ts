/**
 * 终端模块入口
 *
 * 导出终端系统的所有公共接口
 */

// 类型定义
export * from './types';

// CWD 追踪器
export { CwdTracker } from './cwd-tracker';

// 进程轮询管理器
export { ProcessPollingManager } from './process-polling-manager';

// 完成检测策略
export { VSCodeEventsStrategy } from './vscode-events-strategy';
export { ScriptCaptureStrategy } from './script-capture-strategy';
