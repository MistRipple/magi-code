/**
 * Worker Profile System - 引导注入器
 *
 * 核心功能：
 * - 根据 Worker 画像生成 Prompt 前缀
 * - 通过 Prompt 引导 Worker 行为（而非限制工具）
 * - 支持协作上下文注入
 */

import { CLIType } from '../../types';
import { WorkerProfile, InjectionContext } from './types';

export class GuidanceInjector {
  /**
   * 构建 Worker Prompt 前缀
   * 这是核心：通过 Prompt 引导 Worker 行为，而不是限制工具
   */
  buildWorkerPrompt(
    profile: WorkerProfile,
    context: InjectionContext
  ): string {
    const sections: string[] = [];

    // 1. 角色定位
    sections.push(this.buildRoleSection(profile));

    // 2. 专注领域
    if (profile.guidance.focus.length > 0) {
      sections.push(this.buildFocusSection(profile));
    }

    // 3. 行为约束（建议性）
    if (profile.guidance.constraints.length > 0) {
      sections.push(this.buildConstraintsSection(profile));
    }

    // 4. 协作上下文
    if (context.collaborators && context.collaborators.length > 0) {
      sections.push(this.buildCollaborationSection(profile, context));
    }

    // 5. 功能契约
    if (context.featureContract) {
      sections.push(this.buildContractSection(context.featureContract));
    }

    // 6. 输出格式偏好
    if (profile.guidance.outputPreferences.length > 0) {
      sections.push(this.buildOutputSection(profile));
    }

    return sections.join('\n\n');
  }

  /**
   * 构建角色定位部分
   */
  private buildRoleSection(profile: WorkerProfile): string {
    return `## 角色定位\n${profile.guidance.role.trim()}`;
  }

  /**
   * 构建专注领域部分
   */
  private buildFocusSection(profile: WorkerProfile): string {
    const items = profile.guidance.focus.map(f => `- ${f}`).join('\n');
    return `## 专注领域\n${items}`;
  }

  /**
   * 构建行为约束部分
   */
  private buildConstraintsSection(profile: WorkerProfile): string {
    const items = profile.guidance.constraints.map(c => `- ${c}`).join('\n');
    return `## 注意事项\n${items}`;
  }

  /**
   * 构建协作规则部分
   */
  private buildCollaborationSection(
    profile: WorkerProfile,
    context: InjectionContext
  ): string {
    const isLeader = this.isLeaderRole(profile, context);
    const rules = isLeader
      ? profile.collaboration.asLeader
      : profile.collaboration.asCollaborator;

    if (rules.length === 0) return '';

    const roleType = isLeader ? '主导者' : '协作者';
    const items = rules.map(r => `- ${r}`).join('\n');
    return `## 协作规则（${roleType}）\n${items}`;
  }

  /**
   * 构建功能契约部分
   */
  private buildContractSection(featureContract: string): string {
    return `## 功能契约\n${featureContract}`;
  }

  /**
   * 构建输出格式部分
   */
  private buildOutputSection(profile: WorkerProfile): string {
    const items = profile.guidance.outputPreferences.map(p => `- ${p}`).join('\n');
    return `## 输出要求\n${items}`;
  }

  /**
   * 判断是否是主导角色
   */
  private isLeaderRole(
    profile: WorkerProfile,
    context: InjectionContext
  ): boolean {
    // 如果任务分类匹配 Worker 的优先分类，则为主导
    if (context.category) {
      return profile.preferences.preferredCategories.includes(context.category);
    }

    // 基于任务描述关键词判断
    const taskDesc = context.taskDescription.toLowerCase();
    return profile.preferences.preferredCategories.some(cat =>
      taskDesc.includes(cat)
    );
  }

  /**
   * 构建完整的任务 Prompt
   * 组合引导 Prompt + 上下文 + 任务描述
   */
  buildFullTaskPrompt(
    profile: WorkerProfile,
    context: InjectionContext,
    additionalContext?: string
  ): string {
    const sections: string[] = [];

    // 1. 引导 Prompt
    const guidancePrompt = this.buildWorkerPrompt(profile, context);
    sections.push(guidancePrompt);

    // 2. 项目上下文（如果有）
    if (additionalContext) {
      sections.push(`## 项目上下文\n${additionalContext}`);
    }

    // 3. 当前任务
    sections.push(`## 当前任务\n${context.taskDescription}`);

    // 4. 目标文件（如果有）
    if (context.targetFiles && context.targetFiles.length > 0) {
      const files = context.targetFiles.map(f => `- ${f}`).join('\n');
      sections.push(`## 目标文件\n${files}`);
    }

    // 5. 依赖任务（如果有）
    if (context.dependencies && context.dependencies.length > 0) {
      const deps = context.dependencies.map(d => `- ${d}`).join('\n');
      sections.push(`## 依赖任务\n${deps}`);
    }

    return sections.join('\n\n---\n\n');
  }
}

