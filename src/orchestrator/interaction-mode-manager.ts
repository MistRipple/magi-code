/**
 * Interaction Mode Manager - 交互模式管理器
 *
 * 职责：
 * - 管理交互模式 (auto/agent/ask)
 * - 判断是否使用 ask 模式
 * - 同步计划确认策略
 * - 同步恢复确认回调
 */

import {
  InteractionMode,
  INTERACTION_MODE_CONFIGS,
  InteractionModeConfig,
  StrategyConfig,
} from '../types';
import { logger, LogCategory } from '../logging';
import { globalEventBus } from '../events';
import {
  MissionDrivenEngine,
  MissionRecoveryConfirmationCallback,
} from './core';

/**
 * 交互模式管理器
 */
export class InteractionModeManager {
  private interactionMode: InteractionMode = 'auto';
  private modeConfig: InteractionModeConfig = INTERACTION_MODE_CONFIGS.auto;
  private recoveryConfirmationCallback: MissionRecoveryConfirmationCallback | null = null;

  // 关键字列表
  private readonly directAnswerKeywords = [
    '是什么', '为什么', '怎么', '如何', '能否', '可以吗', '建议', '解释', '了解', '对比', '优缺点',
    '方案', '思路', '总结', '概念', '原理', '问题', '是否', '推荐',
    '你能', '你可以', '你会', '能不能', '能吗', '支持吗', '可以', '能否'
  ];

  private readonly taskIntentKeywords = [
    '实现', '添加', '新增', '修改', '修复', '重构', '迁移', '集成', '优化', '部署', '测试', '生成',
    '创建', '删除', '更新', '写', '改', '开发', '搭建', '编排', '完善'
  ];

  constructor(private strategyConfig: StrategyConfig) {}

  /**
   * 设置交互模式
   */
  setInteractionMode(mode: InteractionMode): void {
    this.interactionMode = mode;
    this.modeConfig = INTERACTION_MODE_CONFIGS[mode];
    logger.info('编排器.交互_模式.变更', { mode }, LogCategory.ORCHESTRATOR);
    globalEventBus.emitEvent('orchestrator:mode_changed', { data: { mode } });
  }

  /**
   * 获取当前交互模式
   */
  getInteractionMode(): InteractionMode {
    return this.interactionMode;
  }

  /**
   * 获取当前模式配置
   */
  getModeConfig(): InteractionModeConfig {
    return this.modeConfig;
  }

  /**
   * 设置恢复确认回调
   */
  setRecoveryConfirmationCallback(callback: MissionRecoveryConfirmationCallback): void {
    this.recoveryConfirmationCallback = callback;
  }

  /**
   * 获取恢复确认回调
   */
  getRecoveryConfirmationCallback(): MissionRecoveryConfirmationCallback | null {
    return this.recoveryConfirmationCallback;
  }

  /**
   * 判断是否使用 ask 模式
   */
  shouldUseAskMode(prompt: string): boolean {
    const trimmed = prompt.trim();
    if (!trimmed) return true;

    // 强制模式
    if (this.interactionMode === 'agent') return false;
    if (this.interactionMode === 'ask') return true;

    // 命令前缀
    if (trimmed.startsWith('/agent') || trimmed.startsWith('/task')) return false;

    const lower = trimmed.toLowerCase();

    // 包含代码块或文件路径
    if (lower.includes('```') || /[\\/].+\.\w+/.test(lower)) return false;

    // 检查任务意图
    const hasTaskIntent = this.taskIntentKeywords.some(k => trimmed.includes(k));
    const hasBuildVerb = /(做|制作|搭建|实现|开发|修复|重构|新增|优化|编写|添加|修改)/.test(trimmed);
    const hasBuildTarget = /(功能|页面|模块|接口|系统|组件|服务|项目|API|后端|前端|UI|界面)/i.test(trimmed);

    // 能力询问检测
    const capabilityPattern = /(你能|你可以|你会|能不能|能否|是否|可以|支持)/;
    const endsWithQuestionWord = /(吗|么|？|\?)$/.test(trimmed);
    const hasCapabilityQuestion = capabilityPattern.test(trimmed)
      && (endsWithQuestionWord || /(能做|能否做|可以做)/.test(trimmed))
      && !hasBuildTarget
      && !/(代码|文件|改动|实现|开发|修复|重构|新增|优化)/.test(trimmed);

    if (hasCapabilityQuestion) return true;

    // 结构化任务意图
    const hasStructuredTaskIntent = hasTaskIntent || (hasBuildVerb && hasBuildTarget);
    if (hasStructuredTaskIntent) return false;

    // 问题和直接回答意图
    const hasQuestion = trimmed.includes('?') || trimmed.includes('？');
    const hasDirectAnswerIntent = this.directAnswerKeywords.some(k => trimmed.includes(k));
    const shortPrompt = trimmed.length <= 50;

    return hasQuestion || hasDirectAnswerIntent || shortPrompt;
  }

  /**
   * 同步计划确认策略到引擎
   */
  syncPlanConfirmationPolicy(engine: MissionDrivenEngine): void {
    engine.setPlanConfirmationPolicy((_risk) => {
      if (!this.modeConfig.requirePlanConfirmation) return false;
      return true;
    });
  }

  /**
   * 同步恢复确认回调到引擎
   */
  syncRecoveryConfirmationCallback(engine: MissionDrivenEngine): void {
    const userCallback = this.recoveryConfirmationCallback;
    engine.setRecoveryConfirmationCallback(async (failedTask, error, options) => {
      if (!this.strategyConfig.enableRecovery) {
        return 'continue';
      }

      if (this.modeConfig.autoRollbackOnFailure && this.strategyConfig.autoRollbackOnFailure && options.rollback) {
        return 'rollback';
      }

      if (!this.modeConfig.requireRecoveryConfirmation) {
        if (options.retry) return 'retry';
        if (options.rollback) return 'rollback';
        return 'continue';
      }

      return userCallback
        ? userCallback(failedTask, error, options)
        : (options.retry ? 'retry' : options.rollback ? 'rollback' : 'continue');
    });
  }
}
