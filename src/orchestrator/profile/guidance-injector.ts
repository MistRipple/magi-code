/**
 * Worker Profile System - 引导注入器
 *
 * 核心功能：
 * - 根据 Worker 画像生成 Prompt 前缀
 * - 通过 Prompt 引导 Worker 行为（而非限制工具）
 * - 支持协作上下文注入
 * - 提案 4.3: 6-Section 结构化委托提示
 * - 提案 4.4: 分类别约束
 */

import { WorkerSlot } from '../../types/agent-types';
import { WorkerProfile, InjectionContext, CategoryType } from './types';

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

    // 3. 行为约束（建议性）- 支持分类别约束
    if (profile.guidance.constraints.length > 0) {
      sections.push(this.buildConstraintsSection(profile, context.category as CategoryType));
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

    // 7. 工具使用规则（通用）
    sections.push(this.buildToolUsageSection());

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
   * 提案 4.4: 支持分类别约束
   */
  private buildConstraintsSection(
    profile: WorkerProfile,
    category?: CategoryType
  ): string {
    const sections: string[] = [];

    // 1. 通用约束
    const baseItems = profile.guidance.constraints.map(c => `- ${c}`).join('\n');
    sections.push(`## 注意事项\n\n### 通用约束\n${baseItems}`);

    // 2. 分类别专项约束 - 提案 4.4
    if (category) {
      const categoryConstraints = this.getCategoryConstraints(category);
      if (categoryConstraints.length > 0) {
        const categoryItems = categoryConstraints.map(c => `- ${c}`).join('\n');
        sections.push(`### ${category} 任务专项约束\n${categoryItems}`);
      }
    }

    return sections.join('\n\n');
  }

  /**
   * 获取分类别约束 - 提案 4.4
   */
  private getCategoryConstraints(category: CategoryType): string[] {
    const presets: Record<string, string[]> = {
      'bugfix': [
        '只修复指定的 bug，不要顺便重构',
        '不要添加新功能',
        '保持代码风格一致',
        '确保修复不引入新问题',
      ],
      'refactor': [
        '不要改变外部行为',
        '每次只重构一个模块',
        '重构前确保测试覆盖',
      ],
      'review': [
        '只分析代码，不要修改',
        '关注逻辑问题而非风格问题',
        '提供具体的改进建议',
      ],
      'feature': [
        '遵循现有代码模式',
        '添加必要的错误处理',
        '考虑边界情况',
        '不要过度设计',
      ],
      'test': [
        '测试行为而非实现',
        '覆盖边界情况',
        '保持测试独立性',
        '使用有意义的测试名称',
      ],
      'documentation': [
        '保持文档与代码同步',
        '使用清晰的语言',
        '包含必要的示例',
      ],
      'optimization': [
        '先测量再优化',
        '不要过早优化',
        '保持代码可读性',
        '记录优化理由',
      ],
      'security': [
        '遵循安全最佳实践',
        '验证所有输入',
        '不要在代码中硬编码敏感信息',
        '使用安全的加密算法',
      ],
      'migration': [
        '创建回滚计划',
        '分阶段迁移',
        '保持数据完整性',
        '验证迁移结果',
      ],
      'config': [
        '使用环境变量管理敏感配置',
        '提供合理的默认值',
        '文档化配置选项',
      ],
      'integration': [
        '使用适配器模式隔离外部依赖',
        '处理网络超时和重试',
        '记录集成日志',
      ],
      'general': [],
    };

    return presets[category] || presets['general'] || [];
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
   * 构建工具使用规则（通用）
   */
  private buildToolUsageSection(): string {
    return `## 工具使用\n- 涉及代码/文件修改时，必须使用工具直接编辑文件并保存结果\n- 若无法使用工具完成修改，需明确说明原因并停止\n- 修改完成后需简要说明改动要点`;
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
   * 提案 4.3: 支持 TaskStructuredInfo
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

    // 2. 任务结构化信息 - 提案 4.3
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
   * 构建任务结构化信息部分 - 提案 4.3
   */
  private buildTaskStructuredSection(taskInfo: TaskStructuredInfo): string | null {
    const parts: string[] = [];

    // 预期结果
    if (taskInfo.expectedOutcome && taskInfo.expectedOutcome.length > 0) {
      parts.push(`## 预期结果\n${taskInfo.expectedOutcome.map(o => `- ${o}`).join('\n')}`);
    }

    // 必须做
    if (taskInfo.mustDo && taskInfo.mustDo.length > 0) {
      parts.push(`## 必须遵守\n${taskInfo.mustDo.map(m => `- ${m}`).join('\n')}`);
    }

    // 禁止做
    if (taskInfo.mustNotDo && taskInfo.mustNotDo.length > 0) {
      parts.push(`## 禁止行为\n${taskInfo.mustNotDo.map(m => `- ❌ ${m}`).join('\n')}`);
    }

    // 相关决策
    if (taskInfo.relatedDecisions && taskInfo.relatedDecisions.length > 0) {
      parts.push(`## 已做决策\n${taskInfo.relatedDecisions.map(d => `- ✓ ${d}`).join('\n')}`);
    }

    // 待解决问题
    if (taskInfo.pendingIssues && taskInfo.pendingIssues.length > 0) {
      parts.push(`## 需要注意\n${taskInfo.pendingIssues.map(i => `- ⚠️ ${i}`).join('\n')}`);
    }

    return parts.length > 0 ? parts.join('\n\n') : null;
  }

  /**
   * 构建自检引导 Prompt
   * 基于 Worker 的弱项生成定制化的自检清单
   */
  buildSelfCheckGuidance(
    profile: WorkerProfile,
    taskDescription: string
  ): string {
    const sections: string[] = [];

    sections.push('## 自检清单');

    // 1. 基于弱项生成检查项
    if (profile.profile.weaknesses.length > 0) {
      sections.push('### 重点关注（你的潜在弱项）');
      const weaknessChecks = profile.profile.weaknesses.map(weakness => {
        return `- [ ] 检查是否存在 "${weakness}" 相关问题`;
      });
      sections.push(weaknessChecks.join('\n'));
    }

    // 2. 通用检查项
    sections.push('### 通用检查');
    const generalChecks = [
      '- [ ] 代码是否符合项目规范',
      '- [ ] 是否处理了边界情况',
      '- [ ] 错误处理是否完善',
      '- [ ] 是否有潜在的性能问题',
    ];
    sections.push(generalChecks.join('\n'));

    // 3. 基于任务类型的检查
    const taskChecks = this.inferTaskTypeChecks(taskDescription);
    if (taskChecks.length > 0) {
      sections.push('### 任务相关检查');
      sections.push(taskChecks.map(c => `- [ ] ${c}`).join('\n'));
    }

    return sections.join('\n\n');
  }

  /**
   * 构建互检引导 Prompt
   * 利用评审者的专长视角生成评审指导
   */
  buildPeerReviewGuidance(
    reviewerProfile: WorkerProfile,
    executorProfile: WorkerProfile,
    taskDescription: string
  ): string {
    const sections: string[] = [];

    sections.push('## 互检评审指导');

    // 1. 评审者视角
    sections.push(`### 评审者视角（${reviewerProfile.displayName}）`);
    sections.push(`作为 ${reviewerProfile.profile.strengths.join('、')} 方面的专家，请重点关注：`);
    const reviewerFocus = reviewerProfile.profile.strengths.map(strength => {
      return `- ${strength} 相关的实现质量`;
    });
    sections.push(reviewerFocus.join('\n'));

    // 2. 执行者弱项提醒
    if (executorProfile.profile.weaknesses.length > 0) {
      sections.push(`### 执行者潜在弱项（${executorProfile.displayName}）`);
      sections.push('请特别关注以下可能存在问题的领域：');
      const weaknessWarnings = executorProfile.profile.weaknesses.map(weakness => {
        return `- ⚠️ ${weakness}：可能需要额外审查`;
      });
      sections.push(weaknessWarnings.join('\n'));
    }

    // 3. 评审清单
    sections.push('### 评审清单');
    const reviewChecklist = [
      '- [ ] 代码逻辑是否正确',
      '- [ ] 是否符合架构设计',
      '- [ ] 是否有安全隐患',
      '- [ ] 可维护性如何',
      '- [ ] 是否需要补充测试',
    ];
    sections.push(reviewChecklist.join('\n'));

    // 4. 协作建议
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

    // API 相关
    if (desc.includes('api') || desc.includes('接口') || desc.includes('endpoint')) {
      checks.push('API 接口是否符合 RESTful 规范');
      checks.push('请求/响应格式是否正确');
      checks.push('错误码是否完整');
    }

    // 数据相关
    if (desc.includes('数据') || desc.includes('data') || desc.includes('schema')) {
      checks.push('数据结构是否合理');
      checks.push('数据验证是否完善');
      checks.push('是否处理了空值情况');
    }

    // UI 相关
    if (desc.includes('ui') || desc.includes('界面') || desc.includes('组件')) {
      checks.push('UI 是否响应式');
      checks.push('交互是否流畅');
      checks.push('是否考虑了无障碍访问');
    }

    // 测试相关
    if (desc.includes('测试') || desc.includes('test')) {
      checks.push('测试覆盖率是否足够');
      checks.push('边界情况是否覆盖');
      checks.push('测试是否稳定可重复');
    }

    // 重构相关
    if (desc.includes('重构') || desc.includes('refactor')) {
      checks.push('重构是否保持了原有功能');
      checks.push('是否有回归风险');
      checks.push('代码可读性是否提升');
    }

    return checks;
  }
}
