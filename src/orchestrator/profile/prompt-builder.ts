/**
 * PromptBuilder (single prompt assembly)
 */

import { WorkerSlot } from '../../types/agent-types';
import { WorkerPersona, InjectionContext } from './types';
import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';
import { WORKER_PERSONAS } from './builtin/worker-personas';

export class PromptBuilder {
  buildWorkerPrompt(worker: WorkerSlot, context: InjectionContext): string {
    const persona = WORKER_PERSONAS[worker];
    if (!persona) {
      throw new Error(`未知 Worker: ${worker}`);
    }

    const sections: string[] = [];
    sections.push(this.buildRoleSection(persona));

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

    sections.push(this.buildToolUsageSection());

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

  private buildToolUsageSection(): string {
    return `## 工具使用
- 只能使用系统提供的工具，严禁自行创建或假设任何未列出的工具
- 涉及代码/文件修改时，应使用工具直接编辑文件并保存结果
- 直接使用工具行动，不要在行动前做冗长的规划描述
- 修改完成后简要说明改动要点
- 搜索/检索工具用于快速定位目标代码，找到后立即用 text_editor 修改，避免反复搜索同一内容
- 禁止连续多轮仅使用搜索类工具而不产出实际修改`;
  }
}
