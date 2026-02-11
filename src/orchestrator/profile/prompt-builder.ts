/**
 * PromptBuilder (single prompt assembly)
 *
 * 工具信息由 ToolManager.buildToolsSummary() 动态注入，
 * 核心能力由 ProfileLoader 从 assignedCategories 推导后通过 persona.strengths 传入，
 * 不在此处硬编码具体工具名或能力声明。
 */

import { WorkerPersona, InjectionContext } from './types';
import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';

export class PromptBuilder {
  buildWorkerPrompt(persona: WorkerPersona, context: InjectionContext): string {
    const sections: string[] = [];
    sections.push(this.buildRoleSection(persona));

    // 核心能力（从 assignedCategories 推导，由 ProfileLoader 填充到 persona.strengths）
    if (persona.strengths.length > 0) {
      sections.push(`## 核心能力\n${persona.strengths.map(s => `- ${s}`).join('\n')}`);
    }

    if (context.category) {
      const categoryDef = CATEGORY_DEFINITIONS[context.category];
      if (!categoryDef) {
        throw new Error(`未知分类: ${context.category}`);
      }

      sections.push(`## 任务类型\n${categoryDef.displayName}`);

      if (categoryDef.guidance.focus.length > 0) {
        sections.push(`## 专注领域\n${categoryDef.guidance.focus.map(f => `- ${f}`).join('\n')}`);
      }

      if (categoryDef.guidance.constraints.length > 0) {
        sections.push(`## 行为约束\n${categoryDef.guidance.constraints.map(c => `- ${c}`).join('\n')}`);
      }
    }

    if (context.collaborators && context.collaborators.length > 0) {
      sections.push(`## 协作规则\n${this.buildCollaborationSection(persona, context)}`);
    }

    const reasoningGuidelines = persona.reasoningGuidelines ?? [];
    if (reasoningGuidelines.length > 0) {
      sections.push(`## 推理过程\n${reasoningGuidelines.map(r => `- ${r}`).join('\n')}`);
    }

    const outputPreferences = persona.outputPreferences ?? [];
    if (outputPreferences.length > 0) {
      sections.push(`## 输出要求\n${outputPreferences.map(p => `- ${p}`).join('\n')}`);
    }

    sections.push(this.buildToolUsageSection(context.availableToolsSummary));

    return sections.join('\n\n');
  }

  buildRoleSection(persona: WorkerPersona): string {
    return `## 角色定位\n${persona.baseRole.trim()}`;
  }

  private buildCollaborationSection(persona: WorkerPersona, context: InjectionContext): string {
    const isLeader = context.isLeader === true;
    const collaboration = persona.collaboration ?? { asLeader: [], asCollaborator: [] };
    const rules = isLeader
      ? (collaboration.asLeader ?? [])
      : (collaboration.asCollaborator ?? []);
    if (rules.length === 0) {
      return '';
    }
    const roleType = isLeader ? '主导者' : '协作者';
    return `### ${roleType}\n${rules.map(r => `- ${r}`).join('\n')}`;
  }

  /**
   * 构建工具使用规范段落
   *
   * 可用工具列表由 ToolManager.buildToolsSummary() 动态生成并注入，
   * 此处只定义工具使用策略（工作流 + 禁止行为），不硬编码具体工具名。
   */
  private buildToolUsageSection(toolsSummary?: string): string {
    const sections: string[] = [];

    sections.push('## 工具使用规范');

    // 动态工具列表（内置 + MCP + Skill）
    if (toolsSummary?.trim()) {
      sections.push(`### 可用工具\n${toolsSummary}`);
    }

    // 工具使用策略（与具体工具名解耦）
    sections.push(`### 工作流
1. **定位**（1-2 轮）：通过语义搜索或文本匹配找到目标代码
2. **查看**（1 轮）：读取目标文件，确认要修改的内容
3. **修改**（N 轮）：使用精确替换逐处修改
4. **完成**：输出简短摘要

### 禁止行为
- 禁止用终端命令执行文件读取、目录浏览、内容搜索等操作——使用对应的专用工具
- 禁止输出未经工具执行的代码块（所有修改通过文件编辑工具完成）
- 禁止在每轮工具调用前做冗长的"接下来我将..."规划描述`);

    return sections.join('\n\n');
  }
}
