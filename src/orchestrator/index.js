"use strict";
/**
 * Orchestrator 模块导出
 * 独立编排者架构
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
exports.formatPlanForUser = exports.buildSummaryPrompt = exports.buildExecutionPrompt = exports.buildTaskAnalysisPrompt = exports.RecoveryHandler = exports.VerificationRunner = exports.TaskStateManager = exports.globalMessageBus = exports.MessageBus = exports.WorkerPool = exports.WorkerAgent = exports.OrchestratorAgent = exports.IntelligentOrchestrator = void 0;
// 核心编排器
var intelligent_orchestrator_1 = require("./intelligent-orchestrator");
Object.defineProperty(exports, "IntelligentOrchestrator", { enumerable: true, get: function () { return intelligent_orchestrator_1.IntelligentOrchestrator; } });
// 独立编排者 Agent
var orchestrator_agent_1 = require("./orchestrator-agent");
Object.defineProperty(exports, "OrchestratorAgent", { enumerable: true, get: function () { return orchestrator_agent_1.OrchestratorAgent; } });
// Worker Agent
var worker_agent_1 = require("./worker-agent");
Object.defineProperty(exports, "WorkerAgent", { enumerable: true, get: function () { return worker_agent_1.WorkerAgent; } });
// Worker Pool
var worker_pool_1 = require("./worker-pool");
Object.defineProperty(exports, "WorkerPool", { enumerable: true, get: function () { return worker_pool_1.WorkerPool; } });
// 消息总线
var message_bus_1 = require("./message-bus");
Object.defineProperty(exports, "MessageBus", { enumerable: true, get: function () { return message_bus_1.MessageBus; } });
Object.defineProperty(exports, "globalMessageBus", { enumerable: true, get: function () { return message_bus_1.globalMessageBus; } });
// 协议类型
__exportStar(require("./protocols"), exports);
// 编排者专用 Prompts
__exportStar(require("./prompts/orchestrator-prompts"), exports);
// 任务状态管理
var task_state_manager_1 = require("./task-state-manager");
Object.defineProperty(exports, "TaskStateManager", { enumerable: true, get: function () { return task_state_manager_1.TaskStateManager; } });
// 验证执行器
var verification_runner_1 = require("./verification-runner");
Object.defineProperty(exports, "VerificationRunner", { enumerable: true, get: function () { return verification_runner_1.VerificationRunner; } });
// 恢复处理器
var recovery_handler_1 = require("./recovery-handler");
Object.defineProperty(exports, "RecoveryHandler", { enumerable: true, get: function () { return recovery_handler_1.RecoveryHandler; } });
// 旧版 Prompt 构建器（向后兼容）
var prompts_1 = require("./prompts");
Object.defineProperty(exports, "buildTaskAnalysisPrompt", { enumerable: true, get: function () { return prompts_1.buildTaskAnalysisPrompt; } });
Object.defineProperty(exports, "buildExecutionPrompt", { enumerable: true, get: function () { return prompts_1.buildExecutionPrompt; } });
Object.defineProperty(exports, "buildSummaryPrompt", { enumerable: true, get: function () { return prompts_1.buildSummaryPrompt; } });
Object.defineProperty(exports, "formatPlanForUser", { enumerable: true, get: function () { return prompts_1.formatPlanForUser; } });
//# sourceMappingURL=index.js.map