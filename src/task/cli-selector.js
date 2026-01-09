"use strict";
/**
 * CLI 选择器
 * 根据任务类型、用户配置和 CLI 可用性选择最佳 CLI
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.CLISelector = void 0;
/** 默认 CLI 技能配置 */
const DEFAULT_SKILLS = {
    architecture: 'claude',
    implement: 'claude',
    refactor: 'claude',
    bugfix: 'codex',
    debug: 'claude',
    frontend: 'gemini',
    test: 'codex',
    document: 'claude',
    review: 'claude',
    general: 'claude',
};
/** CLI 优先级降级顺序 */
const CLI_FALLBACK_ORDER = {
    claude: ['codex', 'gemini'],
    codex: ['claude', 'gemini'],
    gemini: ['claude', 'codex'],
};
/**
 * CLI 选择器类
 */
class CLISelector {
    skills;
    availableCLIs = new Set();
    constructor(skills) {
        this.skills = { ...DEFAULT_SKILLS, ...skills };
    }
    /**
     * 更新可用 CLI 列表
     */
    setAvailableCLIs(clis) {
        this.availableCLIs = new Set(clis);
    }
    /**
     * 更新技能配置
     */
    updateSkills(skills) {
        this.skills = { ...this.skills, ...skills };
    }
    /**
     * 根据任务分析选择最佳 CLI
     */
    select(analysis) {
        const preferred = this.skills[analysis.category] || this.skills.general;
        // 检查首选 CLI 是否可用
        if (this.availableCLIs.has(preferred)) {
            return {
                cli: preferred,
                degraded: false,
                preferred,
                reason: `任务类型 "${analysis.category}" 的首选 CLI`,
            };
        }
        // 降级到备选 CLI
        const fallbacks = CLI_FALLBACK_ORDER[preferred] || [];
        for (const fallback of fallbacks) {
            if (this.availableCLIs.has(fallback)) {
                return {
                    cli: fallback,
                    degraded: true,
                    preferred,
                    reason: `首选 ${preferred} 不可用，降级到 ${fallback}`,
                };
            }
        }
        // 如果没有可用的 CLI，返回首选（让调用者处理错误）
        return {
            cli: preferred,
            degraded: false,
            preferred,
            reason: '没有可用的 CLI，使用默认首选',
        };
    }
    /**
     * 根据任务类型直接选择 CLI
     */
    selectByCategory(category) {
        const preferred = this.skills[category] || this.skills.general;
        if (this.availableCLIs.has(preferred)) {
            return {
                cli: preferred,
                degraded: false,
                preferred,
                reason: `任务类型 "${category}" 的首选 CLI`,
            };
        }
        const fallbacks = CLI_FALLBACK_ORDER[preferred] || [];
        for (const fallback of fallbacks) {
            if (this.availableCLIs.has(fallback)) {
                return {
                    cli: fallback,
                    degraded: true,
                    preferred,
                    reason: `首选 ${preferred} 不可用，降级到 ${fallback}`,
                };
            }
        }
        return {
            cli: preferred,
            degraded: false,
            preferred,
            reason: '没有可用的 CLI，使用默认首选',
        };
    }
    /**
     * 获取当前技能配置
     */
    getSkills() {
        return { ...this.skills };
    }
    /**
     * 获取可用 CLI 列表
     */
    getAvailableCLIs() {
        return Array.from(this.availableCLIs);
    }
}
exports.CLISelector = CLISelector;
//# sourceMappingURL=cli-selector.js.map