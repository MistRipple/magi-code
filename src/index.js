"use strict";
/**
 * CLI Arranger 主导出文件
 */
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __exportStar = (this && this.__exportStar) || function(m, exports) {
    for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports, p)) __createBinding(exports, m, p);
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.deactivate = exports.activate = exports.WebviewProvider = exports.createGeminiWorker = exports.GeminiWorker = exports.createCodexWorker = exports.CodexWorker = exports.createClaudeWorker = exports.ClaudeWorker = exports.BaseWorker = exports.Orchestrator = exports.cliDetector = exports.CLIDetector = exports.DiffGenerator = exports.SnapshotManager = exports.TaskManager = exports.SessionManager = exports.globalEventBus = exports.EventEmitter = void 0;
// 类型导出
__exportStar(require("./types"), exports);
// 事件系统
var events_1 = require("./events");
Object.defineProperty(exports, "EventEmitter", { enumerable: true, get: function () { return events_1.EventEmitter; } });
Object.defineProperty(exports, "globalEventBus", { enumerable: true, get: function () { return events_1.globalEventBus; } });
// 管理器
var session_manager_1 = require("./session-manager");
Object.defineProperty(exports, "SessionManager", { enumerable: true, get: function () { return session_manager_1.SessionManager; } });
var task_manager_1 = require("./task-manager");
Object.defineProperty(exports, "TaskManager", { enumerable: true, get: function () { return task_manager_1.TaskManager; } });
var snapshot_manager_1 = require("./snapshot-manager");
Object.defineProperty(exports, "SnapshotManager", { enumerable: true, get: function () { return snapshot_manager_1.SnapshotManager; } });
var diff_generator_1 = require("./diff-generator");
Object.defineProperty(exports, "DiffGenerator", { enumerable: true, get: function () { return diff_generator_1.DiffGenerator; } });
// CLI 检测器
var cli_detector_1 = require("./cli-detector");
Object.defineProperty(exports, "CLIDetector", { enumerable: true, get: function () { return cli_detector_1.CLIDetector; } });
Object.defineProperty(exports, "cliDetector", { enumerable: true, get: function () { return cli_detector_1.cliDetector; } });
// Orchestrator
var orchestrator_1 = require("./orchestrator");
Object.defineProperty(exports, "Orchestrator", { enumerable: true, get: function () { return orchestrator_1.Orchestrator; } });
// Workers
var workers_1 = require("./workers");
Object.defineProperty(exports, "BaseWorker", { enumerable: true, get: function () { return workers_1.BaseWorker; } });
Object.defineProperty(exports, "ClaudeWorker", { enumerable: true, get: function () { return workers_1.ClaudeWorker; } });
Object.defineProperty(exports, "createClaudeWorker", { enumerable: true, get: function () { return workers_1.createClaudeWorker; } });
Object.defineProperty(exports, "CodexWorker", { enumerable: true, get: function () { return workers_1.CodexWorker; } });
Object.defineProperty(exports, "createCodexWorker", { enumerable: true, get: function () { return workers_1.createCodexWorker; } });
Object.defineProperty(exports, "GeminiWorker", { enumerable: true, get: function () { return workers_1.GeminiWorker; } });
Object.defineProperty(exports, "createGeminiWorker", { enumerable: true, get: function () { return workers_1.createGeminiWorker; } });
// UI
var webview_provider_1 = require("./ui/webview-provider");
Object.defineProperty(exports, "WebviewProvider", { enumerable: true, get: function () { return webview_provider_1.WebviewProvider; } });
// 扩展入口
var extension_1 = require("./extension");
Object.defineProperty(exports, "activate", { enumerable: true, get: function () { return extension_1.activate; } });
Object.defineProperty(exports, "deactivate", { enumerable: true, get: function () { return extension_1.deactivate; } });
//# sourceMappingURL=index.js.map