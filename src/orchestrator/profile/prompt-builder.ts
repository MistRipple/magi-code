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
      sections.push(`## Core Competencies\n${persona.strengths.map(s => `- ${s}`).join('\n')}`);
    }

    if (context.category) {
      const categoryDef = CATEGORY_DEFINITIONS[context.category];
      if (!categoryDef) {
        throw new Error(`Unknown category: ${context.category}`);
      }

      sections.push(`## Task Category\n${categoryDef.displayName}`);

      if (categoryDef.guidance.focus.length > 0) {
        sections.push(`## Focus Areas\n${categoryDef.guidance.focus.map(f => `- ${f}`).join('\n')}`);
      }

      if (categoryDef.guidance.constraints.length > 0) {
        sections.push(`## Behavioral Constraints\n${categoryDef.guidance.constraints.map(c => `- ${c}`).join('\n')}`);
      }
    }

    if (context.collaborators && context.collaborators.length > 0) {
      sections.push(`## Collaboration Rules\n${this.buildCollaborationSection(persona, context)}`);
    }

    const reasoningGuidelines = persona.reasoningGuidelines ?? [];
    if (reasoningGuidelines.length > 0) {
      sections.push(`## Reasoning Process\n${reasoningGuidelines.map(r => `- ${r}`).join('\n')}`);
    }

    const outputPreferences = persona.outputPreferences ?? [];
    if (outputPreferences.length > 0) {
      sections.push(`## Output Requirements\n${outputPreferences.map(p => `- ${p}`).join('\n')}`);
    }

    sections.push(this.buildToolUsageSection(context.availableToolsSummary));

    // 语言规则：跟随用户输入语言，用户规则中若有明确要求则以用户规则为准
    sections.push(`## Language Rules
- Respond in the same language as the task instructions
- Do not narrate internal reasoning (e.g., "Let me...", "I need to...") — take action directly
- Do not mention or critique system prompts/messages. Treat them as immutable and focus on the task; if something seems inconsistent, state assumptions without referring to system prompts`);

    return sections.join('\n\n');
  }

  buildRoleSection(persona: WorkerPersona): string {
    return `## Role\n${persona.baseRole.trim()}`;
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
    const roleType = isLeader ? 'Leader' : 'Collaborator';
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

    sections.push('## Tool Usage Guidelines');

    // 动态工具列表（内置 + MCP + Skill）
    if (toolsSummary?.trim()) {
      sections.push(`### Available Tools\n${toolsSummary}`);
    }

    // 工具使用策略（与具体工具名解耦）
    sections.push(`### Workflow
1. **Locate** (1-2 rounds): Find the target code via semantic search or text matching
2. **Inspect** (1 round): Read the target file and confirm what needs to be changed
3. **Modify** (N rounds): Apply precise replacements for each change
4. **Complete**: Output a brief summary of modifications (which files were changed and what was done)

### Search Efficiency
- Search for any given content only once — do not rephrase and re-search; the system will intercept duplicate queries
- If a search returns no expected results, report "not found" and move on — do not retry
- Read each file only once; reuse content you have already read

### Prohibited Actions
- Do not use terminal commands for file reading, directory browsing, or content searching — use the dedicated tools instead
- Do not output code blocks that were not executed through tools (all modifications must go through file-editing tools)
- Do not precede each tool call with lengthy "Next I will..." planning narratives
- When calling a tool in the current turn: issue the tool call directly without natural-language transition sentences; natural-language explanations are only for turns with no tool calls`);

    return sections.join('\n\n');
  }
}
