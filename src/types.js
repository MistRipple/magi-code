"use strict";
/**
 * CLI Arranger 核心类型定义
 * 版本: 0.3.0
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.INTERACTION_MODE_CONFIGS = exports.DegradationLevel = exports.CLIStatusCode = exports.CLI_ROLES = void 0;
// 预设角色配置
exports.CLI_ROLES = {
    claude: {
        type: 'claude',
        name: '架构师/编排者',
        strengths: ['整体架构搭建', '系统设计', '任务分解', '代码审查', '重构规划'],
        taskAffinity: ['architecture', 'refactor', 'review', 'general'],
        keywords: ['架构', '设计', '重构', '模块', '结构', 'refactor', 'design', 'architecture'],
        priority: 1
    },
    codex: {
        type: 'codex',
        name: '修复专家',
        strengths: ['Bug 修复', '问题排查', '性能调优', '错误处理', '代码调试'],
        taskAffinity: ['bugfix', 'debug', 'implement'],
        keywords: ['修复', 'bug', '报错', 'error', 'fix', '调试', 'debug', '性能', 'performance'],
        priority: 2
    },
    gemini: {
        type: 'gemini',
        name: '前端专家',
        strengths: ['前端 UI/UX', '组件开发', '样式处理', '交互逻辑', '响应式设计'],
        taskAffinity: ['frontend', 'implement', 'test'],
        keywords: ['前端', 'UI', '组件', '样式', 'CSS', 'React', 'Vue', 'component', 'frontend'],
        priority: 3
    }
};
// ============================================
// CLI 状态系统 (更细粒度)
// ============================================
// CLI 详细状态枚举
var CLIStatusCode;
(function (CLIStatusCode) {
    CLIStatusCode["AVAILABLE"] = "AVAILABLE";
    CLIStatusCode["NOT_INSTALLED"] = "NOT_INSTALLED";
    CLIStatusCode["AUTH_FAILED"] = "AUTH_FAILED";
    CLIStatusCode["QUOTA_EXCEEDED"] = "QUOTA_EXCEEDED";
    CLIStatusCode["TIMEOUT"] = "TIMEOUT";
    CLIStatusCode["RUNTIME_ERROR"] = "RUNTIME_ERROR";
    CLIStatusCode["NETWORK_ERROR"] = "NETWORK_ERROR"; // 网络问题
})(CLIStatusCode || (exports.CLIStatusCode = CLIStatusCode = {}));
// 降级等级
var DegradationLevel;
(function (DegradationLevel) {
    DegradationLevel[DegradationLevel["FULL"] = 3] = "FULL";
    DegradationLevel[DegradationLevel["DUAL"] = 2] = "DUAL";
    DegradationLevel[DegradationLevel["SINGLE_CLAUDE"] = 1] = "SINGLE_CLAUDE";
    DegradationLevel[DegradationLevel["SINGLE_OTHER"] = 0.5] = "SINGLE_OTHER";
    DegradationLevel[DegradationLevel["NONE"] = 0] = "NONE"; // 无可用 CLI
})(DegradationLevel || (exports.DegradationLevel = DegradationLevel = {}));
/**
 * 预设交互模式配置
 */
exports.INTERACTION_MODE_CONFIGS = {
    ask: {
        mode: 'ask',
        allowFileModification: false,
        allowCommandExecution: false,
        requirePlanConfirmation: false,
        requireRecoveryConfirmation: false,
        autoRollbackOnFailure: false,
        maxFilesToModify: 0,
    },
    agent: {
        mode: 'agent',
        allowFileModification: true,
        allowCommandExecution: true,
        requirePlanConfirmation: true,
        requireRecoveryConfirmation: true,
        autoRollbackOnFailure: false,
        maxFilesToModify: 0,
    },
    auto: {
        mode: 'auto',
        allowFileModification: true,
        allowCommandExecution: true,
        requirePlanConfirmation: false,
        requireRecoveryConfirmation: false,
        autoRollbackOnFailure: true,
        maxFilesToModify: 0, // 可由用户配置
    },
};
//# sourceMappingURL=types.js.map