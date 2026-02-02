/**
 * Worker Profile System - 引导注入器
 *
 * 核心功能：
 * - 通过 PromptBuilder 生成唯一的引导提示
 * - 保留结构化任务信息、自检与互检引导
 */

import { WorkerProfile, InjectionContext } from './types';
import { PromptBuilder } from './prompt-builder';

// ============================================================================
// 任务结构化信息 - 提案 4.3
// ============================================================================

/**
 * 任务结构化信息
 */
export interface TaskStructuredInfo {
  /** 预期结果 */
  expectedOutcome?: string[];
  /** 必须做 */
  mustDo?: string[];
  /** 禁止做 */
  mustNotDo?: string[];
  /** 相关决策（从 MemoryDocument 获取） */
  relatedDecisions?: string[];
  /** 待解决问题（从 MemoryDocument 获取） */
  pendingIssues?: string[];
}

export class GuidanceInjector {
  private promptBuilder = new PromptBuilder();

  /**
   * 构建 Worker Prompt 前缀（唯一入口）
   */
  buildWorkerPrompt(
    profile: WorkerProfile,
    context: InjectionContext
  ): string {
    const isLeader = this.isLeaderRole(profile, context);
    return this.promptBuilder.buildWorkerPrompt(profile.worker, {
      ...context,
      isLeader,
    });
  }

  /**
   * 构建完整的任务 Prompt
   * 组合引导 Prompt + 上下文 + 任务描述
   */
  buildFullTaskPrompt(
    profile: WorkerProfile,
    context: InjectionContext,
    additionalContext?: string,
    taskInfo?: TaskStructuredInfo
  ): string {
    const sections: string[] = [];

    // 1. 引导 Prompt
    const guidancePrompt = this.buildWorkerPrompt(profile, context);
    sections.push(guidancePrompt);

    // 2. 任务结构化信息
    if (taskInfo) {
      const structuredSection = this.buildTaskStructuredSection(taskInfo);
      if (structuredSection) {
        sections.push(structuredSection);
      }
    }

    // 3. 项目上下文（如果有）
    if (additionalContext) {
      sections.push(`## 项目上下文\n${additionalContext}`);
    }

    // 4. 当前任务
    sections.push(`## 当前任务\n${context.taskDescription}`);

    // 5. 目标文件（如果有）
    if (context.targetFiles && context.targetFiles.length > 0) {
      const files = context.targetFiles.map(f => `- ${f}`).join('\n');
      sections.push(`## 目标文件\n${files}`);
    }

    // 6. 依赖任务（如果有）
    if (context.dependencies && context.dependencies.length > 0) {
      const deps = context.dependencies.map(d => `- ${d}`).join('\n');
      sections.push(`## 依赖任务\n${deps}`);
    }

    return sections.join('\n\n---\n\n');
  }

  /**
   * 构建任务结构化信息部分
   */
  private buildTaskStructuredSection(taskInfo: TaskStructuredInfo): string | null {
    const parts: string[] = [];

    if (taskInfo.expectedOutcome && taskInfo.expectedOutcome.length > 0) {
      parts.push(`## 预期结果\n${taskInfo.expectedOutcome.map(o => `- ${o}`).join('\n')}`);
    }

    if (taskInfo.mustDo && taskInfo.mustDo.length > 0) {
      parts.push(`## 必须遵守\n${taskInfo.mustDo.map(m => `- ${m}`).join('\n')}`);
    }

    if (taskInfo.mustNotDo && taskInfo.mustNotDo.length > 0) {
      parts.push(`## 禁止行为\n${taskInfo.mustNotDo.map(m => `- ❌ ${m}`).join('\n')}`);
    }

    if (taskInfo.relatedDecisions && taskInfo.relatedDecisions.length > 0) {
      parts.push(`## 已做决策\n${taskInfo.relatedDecisions.map(d => `- ✓ ${d}`).join('\n')}`);
    }

    if (taskInfo.pendingIssues && taskInfo.pendingIssues.length > 0) {
      parts.push(`## 需要注意\n${taskInfo.pendingIssues.map(i => `- ⚠️ ${i}`).join('\n')}`);
    }

    return parts.length > 0 ? parts.join('\n\n') : null;
  }

  /**
   * 判断是否是主导角色
   */
  private isLeaderRole(profile: WorkerProfile, context: InjectionContext): boolean {
    if (!context.category) return false;
    return profile.assignedCategories.includes(context.category);
  }

  /**
   * 构建自检引导 Prompt
   */
  buildSelfCheckGuidance(profile: WorkerProfile, taskDescription: string): string {
    const sections: string[] = [];

    sections.push('## 自检清单');

    if (profile.persona.weaknesses.length > 0) {
      sections.push('### 重点关注（你的潜在弱项）');
      const weaknessChecks = profile.persona.weaknesses.map(weakness =>
        `- [ ] 检查是否存在 "${weakness}" 相关问题`
      );
      sections.push(weaknessChecks.join('\n'));
    }

    sections.push('### 通用检查');
    const generalChecks = [
      '- [ ] 代码是否符合项目规范',
      '- [ ] 是否处理了边界情况',
      '- [ ] 错误处理是否完善',
      '- [ ] 是否有潜在的性能问题',
    ];
    sections.push(generalChecks.join('\n'));

    const taskChecks = this.inferTaskTypeChecks(taskDescription);
    if (taskChecks.length > 0) {
      sections.push('### 任务相关检查');
      sections.push(taskChecks.map(c => `- [ ] ${c}`).join('\n'));
    }

    return sections.join('\n\n');
  }

  /**
   * 构建互检引导 Prompt
   */
  buildPeerReviewGuidance(
    reviewerProfile: WorkerProfile,
    executorProfile: WorkerProfile,
    taskDescription: string
  ): string {
    const sections: string[] = [];

    sections.push('## 互检评审指导');

    sections.push(`### 评审者视角（${reviewerProfile.persona.displayName}）`);
    sections.push(`作为 ${reviewerProfile.persona.strengths.join('、')} 方面的专家，请重点关注：`);
    const reviewerFocus = reviewerProfile.persona.strengths.map(strength =>
      `- ${strength} 相关的实现质量`
    );
    sections.push(reviewerFocus.join('\n'));

    if (executorProfile.persona.weaknesses.length > 0) {
      sections.push(`### 执行者潜在弱项（${executorProfile.persona.displayName}）`);
      sections.push('请特别关注以下可能存在问题的领域：');
      const weaknessWarnings = executorProfile.persona.weaknesses.map(weakness =>
        `- ⚠️ ${weakness}：可能需要额外审查`
      );
      sections.push(weaknessWarnings.join('\n'));
    }

    sections.push('### 评审清单');
    const reviewChecklist = [
      '- [ ] 代码逻辑是否正确',
      '- [ ] 是否符合架构设计',
      '- [ ] 是否有安全隐患',
      '- [ ] 可维护性如何',
      '- [ ] 是否需要补充测试',
    ];
    sections.push(reviewChecklist.join('\n'));

    sections.push('### 评审反馈建议');
    sections.push('- 指出问题时请提供具体的改进建议');
    sections.push('- 对于复杂问题，可以提供代码示例');
    sections.push('- 区分"必须修改"和"建议优化"');

    return sections.join('\n\n');
  }

  /**
   * 根据任务描述推断需要的检查项
   */
  private inferTaskTypeChecks(taskDescription: string): string[] {
    const checks: string[] = [];
    const desc = taskDescription.toLowerCase();

    if (desc.includes('api') || desc.includes('接口') || desc.includes('endpoint')) {
      checks.push('API 接口是否符合 RESTful 规范');
      checks.push('请求/响应格式是否正确');
      checks.push('错误码是否完整');
    }

    if (desc.includes('数据') || desc.includes('data') || desc.includes('schema')) {
      checks.push('数据结构是否合理');
      checks.push('数据验证是否完善');
      checks.push('是否处理了空值情况');
    }

    if (desc.includes('ui') || desc.includes('界面') || desc.includes('组件')) {
      checks.push('UI 是否响应式');
      checks.push('交互是否流畅');
      checks.push('是否考虑了无障碍访问');
    }

    if (desc.includes('测试') || desc.includes('test')) {
      checks.push('测试覆盖率是否足够');
      checks.push('边界情况是否覆盖');
      checks.push('测试是否稳定可重复');
    }

    if (desc.includes('重构') || desc.includes('refactor')) {
      checks.push('重构是否保持了原有功能');
      checks.push('是否有回归风险');
      checks.push('代码可读性是否提升');
    }

    return checks;
  }
}
